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
/// System program `AdvanceNonceAccount` instruction tag (u32 LE).
const ADVANCE_NONCE: u32 = 4;

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
    /// Recent blockhash, base58. With a durable nonce this is the nonce's
    /// stored blockhash, not a recent one.
    pub blockhash: &'a str,
    /// Durable nonce path: `(nonce_account, system_program, sysvar)`. The
    /// nonce advance becomes the mandatory first instruction, so of several
    /// prepared sweeps at most one can ever confirm.
    pub nonce: Option<(&'a str, &'a str, &'a str)>,
}

/// Serialize an unsigned single-signer `TransferChecked` transaction.
///
/// Without a nonce the account table is `[owner(w,s), source(w), dest(w),
/// mint(r), program(r)]`, header `(1, 0, 2)`, one instruction with SPL
/// account order `[source, mint, destination, authority]` = `[1, 3, 2, 0]`.
/// With a durable nonce the table gains `nonce(w)` plus the readonly system
/// program and RecentBlockhashes sysvar, header `(1, 0, 4)`, and
/// `AdvanceNonceAccount` is the first instruction as the runtime requires.
pub fn build_unsigned_transfer_checked(t: &SweepTransfer) -> Result<Vec<u8>, ClawErr> {
    let mut keys: Vec<[u8; 32]> = vec![
        pk(t.owner, "owner")?,
        pk(t.source, "source token account")?,
        pk(t.destination, "destination token account")?,
    ];
    let mut readonly: Vec<[u8; 32]> = vec![pk(t.mint, "mint")?, pk(t.token_program, "token program")?];
    if let Some((nonce, system, sysvar)) = t.nonce {
        keys.push(pk(nonce, "nonce account")?);
        readonly.push(pk(system, "system program")?);
        readonly.push(pk(sysvar, "recent blockhashes sysvar")?);
    }
    let readonly_count = readonly.len() as u8;
    keys.extend(readonly);
    let blockhash = pk(t.blockhash, "blockhash")?;

    let mut msg = Vec::with_capacity(3 + 1 + keys.len() * 32 + 32 + 32);
    msg.extend_from_slice(&[1, 0, readonly_count]);
    shortvec(keys.len() as u16, &mut msg);
    for key in &keys {
        msg.extend_from_slice(key);
    }
    msg.extend_from_slice(&blockhash);

    // instruction bodies as (program_index, accounts, data)
    let mut instructions: Vec<(u8, Vec<u8>, Vec<u8>)> = Vec::with_capacity(2);
    if t.nonce.is_some() {
        // indices with nonce: owner 0, source 1, dest 2, nonce 3, mint 4,
        // token program 5, system program 6, sysvar 7
        instructions.push((6, vec![3, 7, 0], ADVANCE_NONCE.to_le_bytes().to_vec()));
    }
    let (mint_ix, program_ix) = if t.nonce.is_some() { (4u8, 5u8) } else { (3u8, 4u8) };
    let mut data = Vec::with_capacity(10);
    data.push(TRANSFER_CHECKED);
    data.extend_from_slice(&t.amount_base.to_le_bytes());
    data.push(t.decimals);
    instructions.push((program_ix, vec![1, mint_ix, 2, 0], data));

    shortvec(instructions.len() as u16, &mut msg);
    for (program, accounts, data) in &instructions {
        msg.push(*program);
        shortvec(accounts.len() as u16, &mut msg);
        msg.extend_from_slice(accounts);
        shortvec(data.len() as u16, &mut msg);
        msg.extend_from_slice(data);
    }

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
