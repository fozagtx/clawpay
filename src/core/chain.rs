//! Chain-facing flows, written against an injectable [`ChainClient`] so the
//! whole decision surface (status transitions, caps, fail-closed paths) runs
//! under plain `cargo test` with a mocked chain.
//!
//! Statelessness rule: the plugin trusts only (a) operator config and (b)
//! what the chain says. Amounts the model passes in are used to *describe*
//! the invoice being checked, never to size a sweep; the sweep base is
//! always re-derived from on-chain credits.

use serde_json::{json, Value};

use crate::core::config::{Config, TokenDef, TOKEN_PROGRAM_ID};
use crate::core::error::ClawErr;
use crate::core::payment::{credit_in_tx, decide, Credit, PayStatus};
use crate::core::{money, msgs, time, tx};

/// Minimal JSON-RPC surface the flows need. `rpc` returns the response's
/// `result` member; `rpc_batch` returns one entry per call, in call order,
/// `Value::Null` for calls that individually failed.
pub trait ChainClient {
    fn rpc(&self, url: &str, method: &str, params: Value) -> Result<Value, String>;
    fn rpc_batch(&self, url: &str, calls: &[(&str, Value)]) -> Result<Vec<Value>, String>;
}

const TX_BATCH: usize = 10;

fn tx_params(signature: &str) -> Value {
    json!([signature, {
        "encoding": "jsonParsed",
        "commitment": "confirmed",
        "maxSupportedTransactionVersion": 0
    }])
}

/// All confirmed credits to `recipient` in `mint` for transactions that
/// include `reference` in their account keys (the Solana Pay discovery path).
pub fn collect_credits(
    chain: &dyn ChainClient,
    cfg: &Config,
    reference: &str,
    recipient: &str,
    mint: &str,
) -> Result<Vec<Credit>, ClawErr> {
    let sigs = chain
        .rpc(
            &cfg.rpc_url,
            "getSignaturesForAddress",
            json!([reference, {"limit": cfg.scan_limit, "commitment": "confirmed"}]),
        )
        .map_err(ClawErr::rpc)?;
    let sigs: Vec<String> = sigs
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|e| e.get("err").map(Value::is_null).unwrap_or(false))
                .filter_map(|e| e.get("signature").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut credits = Vec::new();
    for chunk in sigs.chunks(TX_BATCH) {
        let calls: Vec<(&str, Value)> = chunk
            .iter()
            .map(|s| ("getTransaction", tx_params(s)))
            .collect();
        let results = chain.rpc_batch(&cfg.rpc_url, &calls).map_err(ClawErr::rpc)?;
        for tx_value in &results {
            if let Some(credit) = credit_in_tx(tx_value, recipient, mint) {
                credits.push(credit);
            }
        }
    }
    Ok(credits)
}

/// Token accounts owned by `owner` for `mint`: `(pubkey, balance_base)`,
/// sorted by balance descending.
pub fn token_accounts_of(
    chain: &dyn ChainClient,
    cfg: &Config,
    owner: &str,
    mint: &str,
) -> Result<Vec<(String, u64)>, ClawErr> {
    let result = chain
        .rpc(
            &cfg.rpc_url,
            "getTokenAccountsByOwner",
            json!([owner, {"mint": mint}, {"encoding": "jsonParsed", "commitment": "confirmed"}]),
        )
        .map_err(ClawErr::rpc)?;
    let mut accounts: Vec<(String, u64)> = result
        .get("value")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| {
                    let pubkey = e.get("pubkey")?.as_str()?.to_string();
                    let balance = e
                        .pointer("/account/data/parsed/info/tokenAmount/amount")?
                        .as_str()?
                        .parse::<u64>()
                        .ok()?;
                    Some((pubkey, balance))
                })
                .collect()
        })
        .unwrap_or_default();
    accounts.sort_by_key(|(_, balance)| std::cmp::Reverse(*balance));
    Ok(accounts)
}

/// Total base units credited to `owner` in `mint` since `since_unix`,
/// scanning the owner's token accounts. Bounded by `scan_limit` signatures
/// per account; used for the daily caps.
pub fn received_since(
    chain: &dyn ChainClient,
    cfg: &Config,
    owner: &str,
    mint: &str,
    since_unix: i64,
) -> Result<u64, ClawErr> {
    let mut total: u64 = 0;
    for (account, _balance) in token_accounts_of(chain, cfg, owner, mint)? {
        let sigs = chain
            .rpc(
                &cfg.rpc_url,
                "getSignaturesForAddress",
                json!([account, {"limit": cfg.scan_limit, "commitment": "confirmed"}]),
            )
            .map_err(ClawErr::rpc)?;
        let recent: Vec<String> = sigs
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| e.get("err").map(Value::is_null).unwrap_or(false))
                    .filter(|e| {
                        e.get("blockTime")
                            .and_then(Value::as_i64)
                            .map(|t| t >= since_unix)
                            .unwrap_or(false)
                    })
                    .filter_map(|e| e.get("signature").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        for chunk in recent.chunks(TX_BATCH) {
            let calls: Vec<(&str, Value)> = chunk
                .iter()
                .map(|s| ("getTransaction", tx_params(s)))
                .collect();
            let results = chain.rpc_batch(&cfg.rpc_url, &calls).map_err(ClawErr::rpc)?;
            for tx_value in &results {
                if let Some(credit) = credit_in_tx(tx_value, owner, mint) {
                    if credit.block_time.map(|t| t >= since_unix).unwrap_or(true) {
                        total = total.saturating_add(credit.amount_base);
                    }
                }
            }
        }
    }
    Ok(total)
}

pub struct CheckParams<'a> {
    pub reference: &'a str,
    pub expected_base: u64,
    pub expires_at: i64,
    pub token: &'a TokenDef,
    pub recipient: &'a str,
    pub now_unix: i64,
}

pub struct CheckOutcome {
    pub status: PayStatus,
    pub received_base: u64,
    pub paid_at: Option<i64>,
    pub credits: Vec<Credit>,
}

/// Look the invoice up on-chain and decide its status.
pub fn check_payment(
    chain: &dyn ChainClient,
    cfg: &Config,
    p: &CheckParams,
) -> Result<CheckOutcome, ClawErr> {
    money::validate_pubkey(p.reference)
        .map_err(|e| ClawErr::invalid_args(format!("bad reference: {e}")))?;
    let credits = collect_credits(chain, cfg, p.reference, p.recipient, &p.token.mint)?;
    let received_base = credits
        .iter()
        .fold(0u64, |acc, c| acc.saturating_add(c.amount_base));
    let status = decide(p.expected_base, received_base, p.now_unix, p.expires_at);
    let paid_at = credits.iter().filter_map(|c| c.block_time).max();
    Ok(CheckOutcome { status, received_base, paid_at, credits })
}

/// Render a check outcome as tool output with the pt-BR message.
pub fn check_output(cfg: &Config, p: &CheckParams, out: &CheckOutcome, ref_id: &str) -> Value {
    let d = p.token.decimals;
    let symbol = &p.token.symbol;
    let received_fmt = money::format_amount(out.received_base, d);
    let expected_fmt = money::format_amount(p.expected_base, d);
    let expired = p.now_unix > p.expires_at;

    let message_pt = match out.status {
        PayStatus::Paid => {
            let when = time::format_datetime_pt(
                out.paid_at.unwrap_or(p.now_unix),
                cfg.utc_offset_hours,
            );
            msgs::paid(&received_fmt, symbol, ref_id, &when)
        }
        PayStatus::Pending => msgs::pending(ref_id),
        PayStatus::Expired => msgs::expired(ref_id),
        PayStatus::Partial => {
            let missing =
                money::format_amount(p.expected_base.saturating_sub(out.received_base), d);
            msgs::partial(&received_fmt, &expected_fmt, &missing, symbol, ref_id)
        }
    };

    json!({
        "status": out.status.as_str(),
        "reference": p.reference,
        "reference_id": ref_id,
        "token": symbol,
        "expected_amount": money::url_amount(p.expected_base, d),
        "received_amount": money::url_amount(out.received_base, d),
        "expired": expired,
        "paid_at": out.paid_at,
        "paid_at_local": out.paid_at.map(|t| time::format_datetime_pt(t, cfg.utc_offset_hours)),
        "payer": out.credits.iter().rev().find_map(|c| c.payer.clone()),
        "signatures": out.credits.iter().map(|c| c.signature.clone()).collect::<Vec<_>>(),
        "message_pt": message_pt,
    })
}

pub struct SweepParams<'a> {
    pub reference: &'a str,
    pub expected_base: u64,
    pub expires_at: i64,
    pub token: &'a TokenDef,
    pub pct: u8,
    pub now_unix: i64,
}

/// Prepare an UNSIGNED sweep transaction, all caps enforced fail-closed.
///
/// Order of checks matters and is covered by tests: configuration gates
/// first (destination + non-zero max pct), then the requested percentage,
/// then chain-verified payment, then the chain-verified daily cap, then
/// balances. Any RPC failure anywhere refuses the sweep.
pub fn sweep_yield(
    chain: &dyn ChainClient,
    cfg: &Config,
    p: &SweepParams,
) -> Result<Value, ClawErr> {
    let destination = match (&cfg.yield_destination, cfg.max_sweep_pct) {
        (Some(dest), pct_cap) if pct_cap > 0 => dest.clone(),
        _ => return Err(ClawErr::sweep_disabled()),
    };
    if p.pct == 0 {
        return Err(ClawErr::invalid_args("sweep pct must be at least 1"));
    }
    if p.pct > cfg.max_sweep_pct {
        return Err(ClawErr::sweep_pct_too_high(cfg.max_sweep_pct));
    }
    let recipient = cfg.recipient.clone().ok_or_else(ClawErr::no_recipient)?;

    let ref_id = crate::core::invoice::reference_id(p.reference);

    // Payment must be confirmed on-chain; the model's word is not enough.
    let check = check_payment(
        chain,
        cfg,
        &CheckParams {
            reference: p.reference,
            expected_base: p.expected_base,
            expires_at: p.expires_at,
            token: p.token,
            recipient: &recipient,
            now_unix: p.now_unix,
        },
    )?;
    if check.status != PayStatus::Paid {
        return Err(ClawErr::not_paid_yet(&ref_id));
    }

    let d = p.token.decimals;
    let sweep_base = money::pct_of(check.received_base, p.pct);
    if sweep_base == 0 {
        return Err(ClawErr::invalid_args("sweep amount rounds to zero"));
    }

    // Chain-verified daily cap: everything that arrived at the destination
    // today counts against it. Deliberately conservative: inbound from other
    // sources also counts, which can only make the cap tighter, never looser.
    let cap_base = money::parse_amount(&cfg.daily_sweep_cap, d)
        .map_err(|_| ClawErr::config("daily_sweep_cap is not a valid amount"))?;
    let midnight = time::local_midnight(p.now_unix, cfg.utc_offset_hours);
    let today_in = received_since(chain, cfg, &destination, &p.token.mint, midnight)?;
    if today_in.saturating_add(sweep_base) > cap_base {
        let remaining = cap_base.saturating_sub(today_in);
        return Err(ClawErr::sweep_daily_cap(
            &money::format_amount(remaining, d),
            &p.token.symbol,
        ));
    }

    let sources = token_accounts_of(chain, cfg, &recipient, &p.token.mint)?;
    let (source_account, source_balance) = sources
        .into_iter()
        .next()
        .ok_or_else(|| ClawErr::insufficient_balance(&p.token.symbol))?;
    if source_balance < sweep_base {
        return Err(ClawErr::insufficient_balance(&p.token.symbol));
    }

    let destinations = token_accounts_of(chain, cfg, &destination, &p.token.mint)?;
    let (dest_account, _) = destinations
        .into_iter()
        .next()
        .ok_or_else(|| ClawErr::destination_has_no_account(&p.token.symbol))?;

    let blockhash = chain
        .rpc(
            &cfg.rpc_url,
            "getLatestBlockhash",
            json!([{"commitment": "confirmed"}]),
        )
        .map_err(ClawErr::rpc)?
        .pointer("/value/blockhash")
        .and_then(Value::as_str)
        .ok_or_else(|| ClawErr::rpc("getLatestBlockhash returned no blockhash"))?
        .to_string();

    let wire = tx::build_unsigned_transfer_checked(&tx::SweepTransfer {
        owner: &recipient,
        source: &source_account,
        destination: &dest_account,
        mint: &p.token.mint,
        token_program: TOKEN_PROGRAM_ID,
        amount_base: sweep_base,
        decimals: d,
        blockhash: &blockhash,
    })?;

    use base64::Engine as _;
    let unsigned_tx_base64 = base64::engine::general_purpose::STANDARD.encode(wire);
    let sweep_fmt = money::format_amount(sweep_base, d);

    Ok(json!({
        "status": "sweep_ready",
        "reference_id": ref_id,
        "pct": p.pct,
        "sweep_amount": money::url_amount(sweep_base, d),
        "sweep_amount_formatted": sweep_fmt,
        "token": p.token.symbol,
        "received_amount": money::url_amount(check.received_base, d),
        "source_owner": recipient,
        "source_token_account": source_account,
        "destination_owner": destination,
        "destination_token_account": dest_account,
        "unsigned_tx_base64": unsigned_tx_base64,
        "signing_note": "Transação NÃO assinada (TransferChecked). O blockhash expira em ~1 a 2 minutos; assine e envie logo.",
        "message_pt": msgs::sweep_ready(p.pct, &sweep_fmt, &p.token.symbol, &msgs::short_addr(&destination)),
    }))
}
