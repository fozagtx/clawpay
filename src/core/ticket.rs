//! Invoice tickets: a keyed MAC that binds a sweep to an invoice this plugin
//! actually created.
//!
//! The plugin is stateless, so the ticket is the state: `create_invoice`
//! emits HMAC-SHA256(secret, canonical invoice fields) and `sweep_yield`
//! refuses unless the presented fields recompute to the same MAC. The secret
//! lives only in the jailed operator config; the model relays tickets but
//! cannot mint or alter one.

use sha2::{Digest, Sha256};

const BLOCK: usize = 64;

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

fn canonical(reference: &str, mint: &str, amount_base: u64, expires_at: i64, recipient: &str) -> String {
    format!("clawpay.ticket.v1|{reference}|{mint}|{amount_base}|{expires_at}|{recipient}")
}

pub fn make(
    secret: &str,
    reference: &str,
    mint: &str,
    amount_base: u64,
    expires_at: i64,
    recipient: &str,
) -> String {
    let msg = canonical(reference, mint, amount_base, expires_at, recipient);
    bs58::encode(hmac_sha256(secret.as_bytes(), msg.as_bytes())).into_string()
}

pub fn verify(
    secret: &str,
    ticket: &str,
    reference: &str,
    mint: &str,
    amount_base: u64,
    expires_at: i64,
    recipient: &str,
) -> bool {
    let expected = make(secret, reference, mint, amount_base, expires_at, recipient);
    constant_time_eq(expected.as_bytes(), ticket.trim().as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
