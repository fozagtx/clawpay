//! Tool-facing API: argument parsing and action dispatch.
//!
//! This is the whole surface the wasm glue calls. It is pure aside from the
//! injected [`ChainClient`], entropy, and clock, so every action, including
//! its Portuguese output, is exercised by native `cargo test`.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::core::chain::{self, ChainClient, CheckParams, SweepParams};
use crate::core::config::Config;
use crate::core::error::ClawErr;
use crate::core::invoice::{self, InvoiceRequest};
use crate::core::{money, time};

#[derive(Deserialize)]
struct Args {
    action: String,
    // create_invoice
    amount: Option<String>,
    token: Option<String>,
    recipient: Option<String>,
    description: Option<String>,
    expiry_minutes: Option<u64>,
    // check_payment / sweep_yield
    reference: Option<String>,
    expected_amount: Option<String>,
    expires_at: Option<i64>,
    // sweep_yield
    pct: Option<u8>,
    #[serde(rename = "__config", default)]
    config: HashMap<String, String>,
}

/// Mirror of the WIT `tool-result`, kept independent of wit-bindgen so the
/// core compiles natively.
pub struct ActionResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

impl ActionResult {
    fn ok(value: Value) -> Self {
        Self { success: true, output: value.to_string(), error: None }
    }

    fn fail(err: ClawErr) -> Self {
        let body = serde_json::json!({
            "status": "error",
            "code": err.code,
            "message_pt": err.message_pt,
        });
        Self {
            success: false,
            output: body.to_string(),
            error: Some(format!("{}: {}", err.code, err.detail)),
        }
    }
}

/// Entry point used by the component glue and by tests.
pub fn run(
    args_json: &str,
    chain: &dyn ChainClient,
    entropy: [u8; 32],
    now_unix: i64,
) -> ActionResult {
    let args: Args = match serde_json::from_str(args_json) {
        Ok(a) => a,
        Err(e) => return ActionResult::fail(ClawErr::invalid_args(format!("bad JSON: {e}"))),
    };
    let cfg = match Config::from_section(&args.config) {
        Ok(c) => c,
        Err(e) => return ActionResult::fail(e),
    };

    let outcome = match args.action.as_str() {
        "create_invoice" => create_invoice(&args, &cfg, chain, entropy, now_unix),
        "check_payment" => check_payment(&args, &cfg, chain, now_unix),
        "sweep_yield" => sweep_yield(&args, &cfg, chain, now_unix),
        other => Err(ClawErr::invalid_args(format!("unknown action `{other}`"))),
    };
    match outcome {
        Ok(value) => ActionResult::ok(value),
        Err(e) => ActionResult::fail(e),
    }
}

fn create_invoice(
    args: &Args,
    cfg: &Config,
    chain: &dyn ChainClient,
    entropy: [u8; 32],
    now_unix: i64,
) -> Result<Value, ClawErr> {
    let amount = args
        .amount
        .as_deref()
        .ok_or_else(|| ClawErr::invalid_args("`amount` is required for create_invoice"))?;
    let req = InvoiceRequest {
        amount,
        token: args.token.as_deref(),
        recipient: args.recipient.as_deref(),
        description: args.description.as_deref(),
        expiry_minutes: args.expiry_minutes,
    };
    let inv = invoice::create(&req, cfg, entropy, now_unix)?;

    // Daily received-volume cap (F6): only enforced when the operator set
    // one. Chain-derived so the model cannot talk its way past it; an
    // unreachable RPC therefore refuses creation rather than skipping it.
    if let Some(cap) = &cfg.daily_volume_cap {
        let cap_base = money::parse_amount(cap, inv.token.decimals)
            .map_err(|_| ClawErr::config("daily_volume_cap is not a valid amount"))?;
        let midnight = time::local_midnight(now_unix, cfg.utc_offset_hours);
        let today = chain::received_since(
            chain,
            cfg,
            &inv.recipient,
            &inv.token.mint,
            midnight,
        )?;
        if today.saturating_add(inv.amount_base) > cap_base {
            return Err(ClawErr::daily_limit_reached());
        }
    }

    Ok(inv.to_output(cfg))
}

/// Shared arg resolution for the two chain-lookup actions.
fn lookup_params<'a>(
    args: &'a Args,
    cfg: &'a Config,
) -> Result<(&'a str, u64, i64, &'a crate::core::config::TokenDef), ClawErr> {
    let reference = args
        .reference
        .as_deref()
        .ok_or_else(|| ClawErr::invalid_args("`reference` is required"))?;
    let token = cfg.resolve_token(args.token.as_deref())?;
    let expected = args
        .expected_amount
        .as_deref()
        .ok_or_else(|| ClawErr::invalid_args("`expected_amount` is required"))?;
    let expected_base = money::parse_amount(expected, token.decimals)?;
    if expected_base == 0 {
        return Err(ClawErr::invalid_args("expected_amount must be greater than zero"));
    }
    let expires_at = args
        .expires_at
        .ok_or_else(|| ClawErr::invalid_args("`expires_at` is required"))?;
    Ok((reference, expected_base, expires_at, token))
}

fn check_payment(
    args: &Args,
    cfg: &Config,
    chain: &dyn ChainClient,
    now_unix: i64,
) -> Result<Value, ClawErr> {
    let (reference, expected_base, expires_at, token) = lookup_params(args, cfg)?;
    let recipient = cfg.recipient.clone().ok_or_else(ClawErr::no_recipient)?;
    let params = CheckParams {
        reference,
        expected_base,
        expires_at,
        token,
        recipient: &recipient,
        now_unix,
    };
    let outcome = chain::check_payment(chain, cfg, &params)?;
    let ref_id = invoice::reference_id(reference);
    Ok(chain::check_output(cfg, &params, &outcome, &ref_id))
}

fn sweep_yield(
    args: &Args,
    cfg: &Config,
    chain: &dyn ChainClient,
    now_unix: i64,
) -> Result<Value, ClawErr> {
    let (reference, expected_base, expires_at, token) = lookup_params(args, cfg)?;
    let pct = args
        .pct
        .ok_or_else(|| ClawErr::invalid_args("`pct` is required for sweep_yield"))?;
    chain::sweep_yield(
        chain,
        cfg,
        &SweepParams { reference, expected_base, expires_at, token, pct, now_unix },
    )
}

/// JSON Schema forwarded verbatim to the LLM. `__config` is host-reserved
/// and deliberately not declared.
pub fn parameters_schema() -> String {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["create_invoice", "check_payment", "sweep_yield"],
                "description": "create_invoice: build a Solana Pay charge link. check_payment: look up an existing charge on-chain and report paid/pending/partial/expired. sweep_yield: after a confirmed payment, prepare an UNSIGNED transaction moving a capped percentage to the operator-configured yield wallet."
            },
            "amount": {
                "type": "string",
                "description": "create_invoice: charge amount in token units, e.g. \"150\" or \"150,50\". Brazilian decimal comma accepted."
            },
            "token": {
                "type": "string",
                "description": "Token symbol, e.g. \"USDC\". Omit for the configured default."
            },
            "recipient": {
                "type": "string",
                "description": "create_invoice only: receiving wallet override (base58). Usually omit: the operator-configured wallet is used and overrides are rejected unless the operator enabled them."
            },
            "description": {
                "type": "string",
                "description": "create_invoice: short free-text shown to the payer's wallet, e.g. \"Almoço\"."
            },
            "expiry_minutes": {
                "type": "integer",
                "minimum": 1,
                "description": "create_invoice: validity window in minutes. Omit for the configured default (60)."
            },
            "reference": {
                "type": "string",
                "description": "check_payment / sweep_yield: the `reference` value returned by create_invoice."
            },
            "expected_amount": {
                "type": "string",
                "description": "check_payment / sweep_yield: the invoice amount, as returned by create_invoice in `amount`."
            },
            "expires_at": {
                "type": "integer",
                "description": "check_payment / sweep_yield: the invoice `expires_at` unix timestamp returned by create_invoice."
            },
            "pct": {
                "type": "integer",
                "minimum": 1,
                "maximum": 25,
                "description": "sweep_yield: percentage of the received amount to move to the yield reserve. Hard-capped by operator config; requests above the cap are refused."
            }
        },
        "required": ["action"]
    })
    .to_string()
}
