//! Unsigned Solana transaction serialization (legacy message format).
//!
//! ClawPay never holds keys and never signs: the sweep path produces a
//! wire-format transaction with an empty signature slot, base64-encoded, for
//! the operator's own signer (wallet, hardware signer, or a host-side signing
//! tool) to approve. Building it by hand keeps the dependency tree tiny and
//! the byte layout unit-testable.
//!
//! Wire layout: shortvec(signature_count) ++ signatures ++ message, where
//! message = header(3 bytes) ++ shortvec(account_keys) ++ keys ++ blockhash
//! ++ shortvec(instructions) ++ instructions.

use crate::core::error::ClawErr;
use crate::core::money::validate_pubkey;

/// Solana's compact-u16 ("shortvec") length prefix.
pub fn shortvec(mut n: u16, out: &mut Vec<u8>) {
    loop {
        let mut byte = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
}

/// SPL Token `TransferChecked` instruction tag.
const TRANSFER_CHECKED: u8 = 12;

pub struct SweepTransfer<'a> {
    /// Wallet that owns the source token account; fee payer and sole signer.
    pub owner: &'a str,
    /// Source token account (the merchant's ATA).
    pub source: &'a str,
    /// Destination token account (the yield destination's ATA).
    pub destination: &'a str,
    pub mint: &'a str,
    pub token_program: &'a str,
    pub amount_base: u64,
    pub decimals: u8,
    /// Recent blockhash, base58.
    pub blockhash: &'a str,
}

/// Serialize an unsigned single-signer `TransferChecked` transaction.
///
/// Account table: `[owner(w,s), source(w), dest(w), mint(r), program(r)]`,
/// header `(1, 0, 2)`. Instruction accounts follow the SPL order
/// `[source, mint, destination, authority]` = indices `[1, 3, 2, 0]`.
pub fn build_unsigned_transfer_checked(t: &SweepTransfer) -> Result<Vec<u8>, ClawErr> {
    let keys: [[u8; 32]; 5] = [
        pk(t.owner, "owner")?,
        pk(t.source, "source token account")?,
        pk(t.destination, "destination token account")?,
        pk(t.mint, "mint")?,
        pk(t.token_program, "token program")?,
    ];
    let blockhash = pk(t.blockhash, "blockhash")?;

    let mut msg = Vec::with_capacity(3 + 1 + 5 * 32 + 32 + 16);
    // header: 1 required signature, 0 readonly signed, 2 readonly unsigned
    msg.extend_from_slice(&[1, 0, 2]);
    shortvec(keys.len() as u16, &mut msg);
    for key in &keys {
        msg.extend_from_slice(key);
    }
    msg.extend_from_slice(&blockhash);

    // one instruction
    shortvec(1, &mut msg);
    msg.push(4); // program_id_index -> token program
    let accounts = [1u8, 3, 2, 0];
    shortvec(accounts.len() as u16, &mut msg);
    msg.extend_from_slice(&accounts);
    let mut data = Vec::with_capacity(10);
    data.push(TRANSFER_CHECKED);
    data.extend_from_slice(&t.amount_base.to_le_bytes());
    data.push(t.decimals);
    shortvec(data.len() as u16, &mut msg);
    msg.extend_from_slice(&data);

    // wire transaction: one empty signature slot + message
    let mut tx = Vec::with_capacity(1 + 64 + msg.len());
    shortvec(1, &mut tx);
    tx.extend_from_slice(&[0u8; 64]);
    tx.extend_from_slice(&msg);
    Ok(tx)
}

fn pk(s: &str, what: &str) -> Result<[u8; 32], ClawErr> {
    validate_pubkey(s).map_err(|e| ClawErr::new(
        "invalid_pubkey",
        "Recebi um endereço inválido da rede. Tente novamente em instantes.",
        format!("{what}: {e}"),
    ))
}
