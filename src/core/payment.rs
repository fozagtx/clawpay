//! Payment detection from Solana transaction data.
//!
//! Matching is done on token-balance deltas (`preTokenBalances` /
//! `postTokenBalances` from a `jsonParsed` `getTransaction`), not on
//! instruction shapes: this catches plain `transfer`, `transferChecked`, and
//! CPI-wrapped transfers alike, and it measures what actually arrived rather
//! than what an instruction claimed.

use serde_json::Value;

/// One confirmed inbound transfer credited to the recipient for the mint.
#[derive(Debug, Clone, PartialEq)]
pub struct Credit {
    pub amount_base: u64,
    pub signature: String,
    pub block_time: Option<i64>,
    pub payer: Option<String>,
}

/// Sum the recipient-owned balance delta for `mint` in one transaction.
/// Returns `None` for failed transactions or transactions that do not credit
/// the recipient in that mint.
pub fn credit_in_tx(tx: &Value, recipient: &str, mint: &str) -> Option<Credit> {
    let result = tx.get("result").unwrap_or(tx);
    if result.is_null() {
        return None;
    }
    let meta = result.get("meta")?;
    if !meta.get("err")?.is_null() {
        return None;
    }

    let sum = |key: &str| -> u128 {
        meta.get(key)
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| {
                        e.get("owner").and_then(Value::as_str) == Some(recipient)
                            && e.get("mint").and_then(Value::as_str) == Some(mint)
                    })
                    .filter_map(|e| {
                        e.get("uiTokenAmount")?
                            .get("amount")?
                            .as_str()?
                            .parse::<u128>()
                            .ok()
                    })
                    .sum()
            })
            .unwrap_or(0)
    };

    let pre = sum("preTokenBalances");
    let post = sum("postTokenBalances");
    if post <= pre {
        return None;
    }
    let delta = u64::try_from(post - pre).ok()?;

    let signature = result
        .get("transaction")
        .and_then(|t| t.get("signatures"))
        .and_then(Value::as_array)
        .and_then(|s| s.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // Prefer the owner whose token balance decreased (the actual sender);
    // the fee payer is only a fallback, since relayed or exchange payments
    // are signed by someone other than the person paying.
    let owner_delta = |owner: &str| -> i128 {
        let side = |key: &str| -> i128 {
            meta.get(key)
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|e| {
                            e.get("owner").and_then(Value::as_str) == Some(owner)
                                && e.get("mint").and_then(Value::as_str) == Some(mint)
                        })
                        .filter_map(|e| {
                            e.get("uiTokenAmount")?
                                .get("amount")?
                                .as_str()?
                                .parse::<i128>()
                                .ok()
                        })
                        .sum()
                })
                .unwrap_or(0)
        };
        side("postTokenBalances") - side("preTokenBalances")
    };
    let owners: std::collections::BTreeSet<&str> = meta
        .get("preTokenBalances")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            meta.get("postTokenBalances")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .filter_map(|e| e.get("owner").and_then(Value::as_str))
        .collect();
    let sender = owners
        .iter()
        .find(|o| **o != recipient && owner_delta(o) < 0)
        .map(|o| o.to_string());
    let payer = sender.or_else(|| {
        result
            .get("transaction")
            .and_then(|t| t.get("message"))
            .and_then(|m| m.get("accountKeys"))
            .and_then(Value::as_array)
            .and_then(|keys| {
                keys.iter()
                    .find(|k| k.get("signer").and_then(Value::as_bool) == Some(true))
                    .and_then(|k| k.get("pubkey"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
    });

    Some(Credit {
        amount_base: delta,
        signature,
        block_time: result.get("blockTime").and_then(Value::as_i64),
        payer,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayStatus {
    Pending,
    Paid,
    Partial,
    Expired,
}

impl PayStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PayStatus::Pending => "pending",
            PayStatus::Paid => "paid",
            PayStatus::Partial => "partial",
            PayStatus::Expired => "expired",
        }
    }
}

/// Status decision. Payments already received are never un-received by
/// expiry: an expired invoice with partial money in is reported `partial`
/// (with the expired flag set by the caller), so the merchant can follow up.
pub fn decide(expected_base: u64, received_base: u64, now_unix: i64, expires_at: i64) -> PayStatus {
    if received_base >= expected_base {
        PayStatus::Paid
    } else if received_base > 0 {
        PayStatus::Partial
    } else if now_unix > expires_at {
        PayStatus::Expired
    } else {
        PayStatus::Pending
    }
}
