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
/// `result` member; `rpc_batch` returns one entry per call, in call order.
/// Consumers treat a `Value::Null` entry as that call having FAILED (per-item
/// RPC error, missing id, or a null result for a signature the node itself
/// just returned) and refuse the whole operation: an unverifiable transaction
/// must never count as "paid nothing" in a cap check.
pub trait ChainClient {
    fn rpc(&self, url: &str, method: &str, params: Value) -> Result<Value, String>;
    fn rpc_batch(&self, url: &str, calls: &[(&str, Value)]) -> Result<Vec<Value>, String>;
}

const TX_BATCH: usize = 10;
/// Pagination bound for signature scans: at most this many pages of
/// `scan_limit` signatures per address. Exhausting it while entries are still
/// inside the time window refuses the operation instead of undercounting.
const MAX_SCAN_PAGES: usize = 8;
/// How long after invoice expiry a ticket still authorizes a sweep. Bounds
/// replay of old paid invoices without racing genuinely late sweeps.
const SWEEP_GRACE_SECS: i64 = 24 * 3600;

fn tx_params(signature: &str) -> Value {
    json!([signature, {
        "encoding": "jsonParsed",
        "commitment": "confirmed",
        "maxSupportedTransactionVersion": 0
    }])
}

/// Successful signatures for `address`, newest first, paginated with `before`
/// until history ends or (when `since` is set) an entry older than `since` is
/// reached. Entries with no `blockTime` are kept: unknown age must widen a
/// cap scan, never narrow it. Fails closed when `MAX_SCAN_PAGES` is exhausted
/// while still inside the window.
fn signatures_since(
    chain: &dyn ChainClient,
    cfg: &Config,
    address: &str,
    since: Option<i64>,
) -> Result<Vec<String>, ClawErr> {
    let mut out = Vec::new();
    let mut before: Option<String> = None;
    for _ in 0..MAX_SCAN_PAGES {
        let mut opts = json!({"limit": cfg.scan_limit, "commitment": "confirmed"});
        if let Some(b) = &before {
            opts["before"] = json!(b);
        }
        let page = chain
            .rpc(&cfg.rpc_url, "getSignaturesForAddress", json!([address, opts]))
            .map_err(ClawErr::rpc)?;
        let entries = page
            .as_array()
            .cloned()
            .ok_or_else(|| ClawErr::rpc("signatures result is not an array"))?;
        for e in &entries {
            if let (Some(since), Some(t)) = (since, e.get("blockTime").and_then(Value::as_i64)) {
                if t < since {
                    return Ok(out);
                }
            }
            if e.get("err").map(Value::is_null).unwrap_or(false) {
                if let Some(s) = e.get("signature").and_then(Value::as_str) {
                    out.push(s.to_string());
                }
            }
        }
        if entries.len() < cfg.scan_limit {
            return Ok(out);
        }
        before = entries
            .last()
            .and_then(|e| e.get("signature"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if before.is_none() {
            return Err(ClawErr::rpc("full signatures page without a usable cursor"));
        }
    }
    Err(ClawErr::scan_exhausted())
}

/// Fetch each signature's transaction (batched) and extract credits to
/// `owner` in `mint`. Any per-item failure (Null slot) refuses the whole
/// lookup rather than being counted as zero.
fn fetch_credits(
    chain: &dyn ChainClient,
    cfg: &Config,
    sigs: &[String],
    owner: &str,
    mint: &str,
) -> Result<Vec<Credit>, ClawErr> {
    let mut credits = Vec::new();
    for chunk in sigs.chunks(TX_BATCH) {
        let calls: Vec<(&str, Value)> = chunk
            .iter()
            .map(|s| ("getTransaction", tx_params(s)))
            .collect();
        let results = chain.rpc_batch(&cfg.rpc_url, &calls).map_err(ClawErr::rpc)?;
        if results.len() != chunk.len() {
            return Err(ClawErr::rpc("batch returned wrong number of results"));
        }
        for tx_value in &results {
            if tx_value.is_null() {
                return Err(ClawErr::rpc("per-item getTransaction failure in batch"));
            }
            if let Some(credit) = credit_in_tx(tx_value, owner, mint) {
                credits.push(credit);
            }
        }
    }
    Ok(credits)
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
    let sigs = signatures_since(chain, cfg, reference, None)?;
    fetch_credits(chain, cfg, &sigs, recipient, mint)
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
    // One transaction can touch several of the owner's token accounts; count
    // each signature once or the same credit is summed twice.
    let mut seen = std::collections::BTreeSet::new();
    for (account, _balance) in token_accounts_of(chain, cfg, owner, mint)? {
        let sigs: Vec<String> = signatures_since(chain, cfg, &account, Some(since_unix))?
            .into_iter()
            .filter(|s| seen.insert(s.clone()))
            .collect();
        for credit in fetch_credits(chain, cfg, &sigs, owner, mint)? {
            if credit.block_time.map(|t| t >= since_unix).unwrap_or(true) {
                total = total.saturating_add(credit.amount_base);
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
    /// HMAC ticket issued by `create_invoice`; proves the lookup fields
    /// describe an invoice this plugin created.
    pub ticket: Option<&'a str>,
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
    let (destination, secret) = match (&cfg.yield_destination, cfg.max_sweep_pct, &cfg.invoice_secret)
    {
        (Some(dest), pct_cap, Some(secret)) if pct_cap > 0 => (dest.clone(), secret.clone()),
        _ => return Err(ClawErr::sweep_disabled()),
    };
    if p.pct == 0 {
        return Err(ClawErr::invalid_args("sweep pct must be at least 1"));
    }
    if p.pct > cfg.max_sweep_pct {
        return Err(ClawErr::sweep_pct_too_high(cfg.max_sweep_pct));
    }
    let recipient = cfg.recipient.clone().ok_or_else(ClawErr::no_recipient)?;

    // The model relays lookup fields; the ticket proves they came from a
    // create_invoice call of this plugin and were not altered.
    let ticket = p
        .ticket
        .ok_or_else(|| ClawErr::invalid_args("`ticket` is required for sweep_yield"))?;
    if !crate::core::ticket::verify(
        &secret,
        ticket,
        p.reference,
        &p.token.mint,
        p.expected_base,
        p.expires_at,
        &recipient,
    ) {
        return Err(ClawErr::invalid_ticket());
    }
    if p.now_unix > p.expires_at.saturating_add(SWEEP_GRACE_SECS) {
        return Err(ClawErr::ticket_expired());
    }

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

    // With a durable nonce configured, build on the nonce's stored blockhash
    // and advance it first: of several prepared sweeps, the chain confirms at
    // most one. Otherwise use a recent blockhash (roughly 2 minute validity).
    let (blockhash, nonce) = match &cfg.nonce_account {
        Some(nonce_account) => {
            let info = chain
                .rpc(
                    &cfg.rpc_url,
                    "getAccountInfo",
                    json!([nonce_account, {"encoding": "jsonParsed", "commitment": "confirmed"}]),
                )
                .map_err(ClawErr::rpc)?;
            if info.pointer("/value/data/program").and_then(Value::as_str) != Some("nonce")
                || info.pointer("/value/data/parsed/type").and_then(Value::as_str)
                    != Some("initialized")
            {
                return Err(ClawErr::nonce_misconfigured(
                    "account is not an initialized nonce account",
                ));
            }
            let parsed = info
                .pointer("/value/data/parsed/info")
                .ok_or_else(|| ClawErr::nonce_misconfigured("account missing or not a parsed nonce"))?;
            let authority = parsed.get("authority").and_then(Value::as_str).unwrap_or("");
            if authority != recipient {
                return Err(ClawErr::nonce_misconfigured(
                    "nonce authority is not the recipient wallet",
                ));
            }
            let stored = parsed
                .get("blockhash")
                .and_then(Value::as_str)
                .ok_or_else(|| ClawErr::nonce_misconfigured("nonce has no stored blockhash"))?;
            (
                stored.to_string(),
                Some((
                    nonce_account.clone(),
                    crate::core::config::SYSTEM_PROGRAM_ID,
                    crate::core::config::RECENT_BLOCKHASHES_SYSVAR,
                )),
            )
        }
        None => {
            let recent = chain
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
            (recent, None)
        }
    };

    let wire = tx::build_unsigned_transfer_checked(&tx::SweepTransfer {
        owner: &recipient,
        source: &source_account,
        destination: &dest_account,
        mint: &p.token.mint,
        token_program: TOKEN_PROGRAM_ID,
        amount_base: sweep_base,
        decimals: d,
        blockhash: &blockhash,
        nonce: nonce
            .as_ref()
            .map(|(account, system, sysvar)| (account.as_str(), *system, *sysvar)),
    })?;
    let signing_note = if nonce.is_some() {
        "Transação NÃO assinada (TransferChecked com nonce durável). \
         O nonce garante que só uma reserva preparada pode ser confirmada; \
         assine e envie quando quiser."
    } else {
        "Transação NÃO assinada (TransferChecked). O blockhash expira em ~1 a 2 \
         minutos; assine e envie logo. Prepare e assine uma reserva por vez: o \
         limite diário só conta transações já confirmadas na rede."
    };

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
        "signing_note": signing_note,
        "message_pt": msgs::sweep_ready(p.pct, &sweep_fmt, &p.token.symbol, &msgs::short_addr(&destination)),
    }))
}
