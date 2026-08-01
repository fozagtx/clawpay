//! ClawPay configuration, resolved from the plugin's jailed `__config` section.
//!
//! The host injects a flat `string -> string` map. Every typed field is a
//! parse-with-default, and the defaults are chosen so that an EMPTY map is
//! safe: no recipient means invoice creation refuses, no yield destination
//! means sweeps refuse, and every cap starts at its most conservative value.
//! Nothing in this struct can be supplied or overridden by the model at
//! runtime — the host strips any caller-provided `__config` before injection.

use std::collections::HashMap;

use crate::core::error::ClawErr;
use crate::core::money;

/// SPL Token program (mainnet + devnet).
pub const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// USDC mint on mainnet-beta.
pub const USDC_MAINNET_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Absolute ceiling on the sweep percentage, compiled into the plugin.
/// Operator config can only lower it, never raise it.
pub const HARD_MAX_SWEEP_PCT: u8 = 25;
/// Absolute ceiling on signatures scanned per chain lookup.
pub const HARD_MAX_SCAN_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct TokenDef {
    pub symbol: String,
    pub mint: String,
    pub decimals: u8,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Solana JSON-RPC endpoint.
    pub rpc_url: String,
    /// Merchant wallet that receives invoice payments (base58 pubkey).
    /// No default: without it, invoice creation fails closed.
    pub recipient: Option<String>,
    /// Whether `create_invoice` may take a recipient from tool arguments.
    /// Off by default: a prompt-injected agent must not be able to redirect
    /// invoices to an arbitrary wallet.
    pub allow_recipient_override: bool,
    /// Tokens invoices may be denominated in. First entry is the default.
    pub tokens: Vec<TokenDef>,
    /// Maximum amount per invoice, in token units (e.g. "2000").
    pub max_invoice_amount: String,
    /// Optional daily received-volume cap, in token units. When set, invoice
    /// creation checks on-chain volume received today and fails closed if the
    /// cap is reached (or if the chain cannot be queried).
    pub daily_volume_cap: Option<String>,
    /// Default invoice validity in minutes.
    pub default_expiry_minutes: u64,
    /// Maximum sweep percentage the operator allows. 0 disables sweeps
    /// entirely (the default). Clamped to [`HARD_MAX_SWEEP_PCT`].
    pub max_sweep_pct: u8,
    /// Absolute cap on tokens swept to the yield destination per local day.
    pub daily_sweep_cap: String,
    /// Pre-approved yield destination wallet (base58 pubkey). Config-only;
    /// there is deliberately no way to pass this at runtime.
    pub yield_destination: Option<String>,
    /// Max signatures scanned per reference / per daily-volume lookup.
    pub scan_limit: usize,
    /// Local timezone as UTC offset in hours. Default -3 (Brasília).
    pub utc_offset_hours: i32,
    /// Label shown to the payer's wallet in the Solana Pay URL.
    pub label: String,
}

fn get<'a>(s: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    s.get(k).map(|v| v.trim()).filter(|v| !v.is_empty())
}

fn parse_tokens(raw: &str) -> Result<Vec<TokenDef>, ClawErr> {
    let mut out = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let parts: Vec<&str> = entry.split(':').map(str::trim).collect();
        if parts.len() != 3 {
            return Err(ClawErr::config(format!(
                "allowed_tokens entry `{entry}` must be SYMBOL:MINT:DECIMALS"
            )));
        }
        let decimals: u8 = parts[2]
            .parse()
            .map_err(|_| ClawErr::config(format!("invalid decimals in `{entry}`")))?;
        if decimals > 18 {
            return Err(ClawErr::config(format!("decimals too large in `{entry}`")));
        }
        money::validate_pubkey(parts[1])
            .map_err(|_| ClawErr::config(format!("invalid mint pubkey in `{entry}`")))?;
        out.push(TokenDef {
            symbol: parts[0].to_uppercase(),
            mint: parts[1].to_string(),
            decimals,
        });
    }
    if out.is_empty() {
        return Err(ClawErr::config("allowed_tokens resolved to an empty list"));
    }
    Ok(out)
}

impl Config {
    pub fn from_section(section: &HashMap<String, String>) -> Result<Self, ClawErr> {
        let tokens = match get(section, "allowed_tokens") {
            Some(raw) => parse_tokens(raw)?,
            None => vec![TokenDef {
                symbol: "USDC".to_string(),
                mint: USDC_MAINNET_MINT.to_string(),
                decimals: 6,
            }],
        };

        let recipient = match get(section, "recipient") {
            Some(r) => {
                money::validate_pubkey(r)
                    .map_err(|_| ClawErr::config("recipient is not a valid base58 pubkey"))?;
                Some(r.to_string())
            }
            None => None,
        };

        let yield_destination = match get(section, "yield_destination") {
            Some(r) => {
                money::validate_pubkey(r).map_err(|_| {
                    ClawErr::config("yield_destination is not a valid base58 pubkey")
                })?;
                Some(r.to_string())
            }
            None => None,
        };

        let max_sweep_pct = get(section, "max_sweep_pct")
            .map(|v| {
                v.parse::<u8>()
                    .map_err(|_| ClawErr::config("max_sweep_pct must be an integer 0-25"))
            })
            .transpose()?
            .unwrap_or(0)
            .min(HARD_MAX_SWEEP_PCT);

        let scan_limit = get(section, "scan_limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(20)
            .clamp(1, HARD_MAX_SCAN_LIMIT);

        let utc_offset_hours = get(section, "utc_offset_hours")
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|v| (-12..=14).contains(v))
            .unwrap_or(-3);

        let default_expiry_minutes = get(section, "default_expiry_minutes")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(60)
            .min(60 * 24 * 7);

        Ok(Config {
            rpc_url: get(section, "rpc_url")
                .unwrap_or("https://api.mainnet-beta.solana.com")
                .to_string(),
            recipient,
            allow_recipient_override: get(section, "allow_recipient_override")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            tokens,
            max_invoice_amount: get(section, "max_invoice_amount")
                .unwrap_or("2000")
                .to_string(),
            daily_volume_cap: get(section, "daily_volume_cap").map(str::to_string),
            default_expiry_minutes,
            max_sweep_pct,
            daily_sweep_cap: get(section, "daily_sweep_cap").unwrap_or("500").to_string(),
            yield_destination,
            scan_limit,
            utc_offset_hours,
            label: get(section, "label").unwrap_or("ClawPay").to_string(),
        })
    }

    /// Resolve a token by symbol (case-insensitive); `None` selects the
    /// default (first configured) token. Unknown symbols fail closed.
    pub fn resolve_token(&self, symbol: Option<&str>) -> Result<&TokenDef, ClawErr> {
        match symbol.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(&self.tokens[0]),
            Some(s) => self
                .tokens
                .iter()
                .find(|t| t.symbol.eq_ignore_ascii_case(s))
                .ok_or_else(|| ClawErr::token_not_allowed(s, &self.tokens)),
        }
    }
}
