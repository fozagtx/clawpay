//! Invoice creation: Solana Pay transfer-request URLs with a unique
//! `reference` key.
//!
//! The reference is the invoice's identity. It is a fresh 32-byte value
//! encoded as a base58 pubkey and appended (by the payer's wallet, per the
//! Solana Pay spec) to the transfer transaction's account keys, which makes
//! the payment discoverable later via `getSignaturesForAddress(reference)`.
//! ClawPay itself keeps no state between calls; the chain is the database.

use sha2::{Digest, Sha256};

use crate::core::config::{Config, TokenDef};
use crate::core::error::ClawErr;
use crate::core::{money, msgs, time};

/// Human-friendly reference ID alphabet: no 0/O/1/I/L look-alikes.
const REF_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

pub struct InvoiceRequest<'a> {
    pub amount: &'a str,
    pub token: Option<&'a str>,
    pub recipient: Option<&'a str>,
    pub description: Option<&'a str>,
    pub expiry_minutes: Option<u64>,
}

#[derive(Debug)]
pub struct Invoice {
    pub reference: String,
    pub reference_id: String,
    pub url: String,
    pub amount_base: u64,
    pub token: TokenDef,
    pub recipient: String,
    pub expires_at: i64,
}

/// Derive the short `CP-XXXXX` id shown to humans from the reference pubkey.
pub fn reference_id(reference: &str) -> String {
    let hash = Sha256::digest(reference.as_bytes());
    let mut id = String::from("CP-");
    for byte in hash.iter().take(5) {
        id.push(REF_ALPHABET[*byte as usize % REF_ALPHABET.len()] as char);
    }
    id
}

/// Percent-encode a Solana Pay URL query value (RFC 3986 unreserved kept).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Resolve the invoice recipient. Config wins; an argument recipient is only
/// honored when the operator explicitly allowed overrides.
fn resolve_recipient<'a>(cfg: &'a Config, arg: Option<&'a str>) -> Result<&'a str, ClawErr> {
    match (cfg.recipient.as_deref(), arg.map(str::trim).filter(|s| !s.is_empty())) {
        (Some(configured), None) => Ok(configured),
        (Some(configured), Some(requested)) => {
            if requested == configured {
                Ok(configured)
            } else if cfg.allow_recipient_override {
                money::validate_pubkey(requested)
                    .map_err(|e| ClawErr::invalid_args(format!("bad recipient: {e}")))?;
                Ok(requested)
            } else {
                Err(ClawErr::recipient_override_forbidden())
            }
        }
        (None, Some(requested)) if cfg.allow_recipient_override => {
            money::validate_pubkey(requested)
                .map_err(|e| ClawErr::invalid_args(format!("bad recipient: {e}")))?;
            Ok(requested)
        }
        (None, _) => Err(ClawErr::no_recipient()),
    }
}

/// Build an invoice. Pure: entropy and clock are injected by the caller.
/// Enforces the per-invoice maximum before anything else.
pub fn create(
    req: &InvoiceRequest,
    cfg: &Config,
    entropy: [u8; 32],
    now_unix: i64,
) -> Result<Invoice, ClawErr> {
    let token = cfg.resolve_token(req.token)?.clone();
    let recipient = resolve_recipient(cfg, req.recipient)?.to_string();

    let amount_base = money::parse_amount(req.amount, token.decimals)?;
    if amount_base == 0 {
        return Err(ClawErr::invalid_args("amount must be greater than zero"));
    }
    let max_base = money::parse_amount(&cfg.max_invoice_amount, token.decimals)
        .map_err(|_| ClawErr::config("max_invoice_amount is not a valid amount"))?;
    if amount_base > max_base {
        return Err(ClawErr::amount_too_high(
            &money::format_amount(max_base, token.decimals),
            &token.symbol,
        ));
    }

    let reference = bs58::encode(entropy).into_string();
    let reference_id = reference_id(&reference);

    // Clamp to the same one-week bound as the operator default: the argument
    // comes from the model and must not overflow the timestamp arithmetic.
    let expiry_minutes = req
        .expiry_minutes
        .filter(|m| *m > 0)
        .map(|m| m.min(60 * 24 * 7))
        .unwrap_or(cfg.default_expiry_minutes);
    let expires_at = now_unix + (expiry_minutes as i64) * 60;

    let mut url = format!(
        "solana:{recipient}?amount={}&spl-token={}&reference={reference}",
        money::url_amount(amount_base, token.decimals),
        token.mint,
    );
    url.push_str(&format!("&label={}", encode(&cfg.label)));
    if let Some(desc) = req.description.map(str::trim).filter(|d| !d.is_empty()) {
        url.push_str(&format!("&message={}", encode(desc)));
    }
    url.push_str(&format!("&memo={}", encode(&reference_id)));

    Ok(Invoice {
        reference,
        reference_id,
        url,
        amount_base,
        token,
        recipient,
        expires_at,
    })
}

impl Invoice {
    /// Structured JSON the agent stores and later feeds back to
    /// `check_payment` / `sweep_yield`, plus the ready pt-BR message.
    pub fn to_output(&self, cfg: &Config) -> serde_json::Value {
        let amount_fmt = money::format_amount(self.amount_base, self.token.decimals);
        let valid_until = time::format_deadline_pt(self.expires_at, cfg.utc_offset_hours);
        serde_json::json!({
            "status": "created",
            "reference": self.reference,
            "reference_id": self.reference_id,
            "url": self.url,
            "qr_content": self.url,
            "amount": money::url_amount(self.amount_base, self.token.decimals),
            "amount_formatted": amount_fmt,
            "token": self.token.symbol,
            "mint": self.token.mint,
            "decimals": self.token.decimals,
            "recipient": self.recipient,
            "expires_at": self.expires_at,
            "expires_at_local": valid_until,
            "message_pt": msgs::created(&amount_fmt, &self.token.symbol, &self.url, &self.reference_id, &valid_until),
        })
    }
}
