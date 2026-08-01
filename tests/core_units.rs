//! Unit tests for the leaf modules: money, time, payment decisions, wire
//! serialization, and config defaults.

mod common;

use std::collections::HashMap;

use clawpay::core::config::{Config, HARD_MAX_SWEEP_PCT};
use clawpay::core::money::{format_amount, parse_amount, pct_of, url_amount};
use clawpay::core::payment::{credit_in_tx, decide, PayStatus};
use clawpay::core::time::{format_datetime_pt, format_deadline_pt, local_midnight};
use clawpay::core::tx::{build_unsigned_transfer_checked, shortvec, SweepTransfer};

// ---------------------------------------------------------------- money

#[test]
fn parses_plain_and_brazilian_amounts() {
    assert_eq!(parse_amount("150", 6).unwrap(), 150_000_000);
    assert_eq!(parse_amount("150,25", 6).unwrap(), 150_250_000);
    assert_eq!(parse_amount("150.25", 6).unwrap(), 150_250_000);
    assert_eq!(parse_amount("1.234,56", 6).unwrap(), 1_234_560_000);
    assert_eq!(parse_amount("1,234.56", 6).unwrap(), 1_234_560_000);
    assert_eq!(parse_amount("0,50", 6).unwrap(), 500_000);
    assert_eq!(parse_amount(" 2 000 ", 6).unwrap(), 2_000_000_000);
    assert_eq!(parse_amount("1.234.567,89", 6).unwrap(), 1_234_567_890_000);
}

#[test]
fn rejects_bad_amounts() {
    assert!(parse_amount("", 6).is_err());
    assert!(parse_amount("-5", 6).is_err());
    assert!(parse_amount("abc", 6).is_err());
    assert!(parse_amount("1,2345678", 6).is_err()); // more precision than mint
    assert!(parse_amount("99999999999999999999999", 6).is_err()); // overflow
    // mixed separators must be properly grouped, not guessed
    assert!(parse_amount("12.34,56", 6).is_err());
    assert!(parse_amount("1,234.56.78", 6).is_err());
    assert!(parse_amount("1234.567,89", 6).is_err());
}

#[test]
fn formats_brazilian_style() {
    assert_eq!(format_amount(150_000_000, 6), "150,00");
    assert_eq!(format_amount(1_234_560_000, 6), "1.234,56");
    assert_eq!(format_amount(500_000, 6), "0,50");
    assert_eq!(format_amount(1_000_123, 6), "1,000123");
    assert_eq!(format_amount(0, 6), "0,00");
    assert_eq!(format_amount(15, 1), "1,50"); // always at least two places
}

#[test]
fn url_amounts_are_canonical() {
    assert_eq!(url_amount(150_000_000, 6), "150");
    assert_eq!(url_amount(150_500_000, 6), "150.5");
    assert_eq!(url_amount(1, 6), "0.000001");
}

#[test]
fn pct_floors_and_never_overflows() {
    assert_eq!(pct_of(150_000_000, 10), 15_000_000);
    assert_eq!(pct_of(1, 10), 0);
    assert_eq!(pct_of(u64::MAX, 25), u64::MAX / 4);
}

// ---------------------------------------------------------------- time

#[test]
fn midnight_and_formatting_in_brasilia() {
    // 2026-08-01 14:32 UTC-3 == 17:32 UTC == unix 1785605520
    let t = 1_785_605_520;
    assert_eq!(format_datetime_pt(t, -3), "01/08/2026 às 14:32");
    assert_eq!(format_deadline_pt(t, -3), "14:32 de 01/08/2026");
    let midnight = local_midnight(t, -3);
    // local midnight 2026-08-01 00:00 -03 == 03:00 UTC == 1785553200
    assert_eq!(midnight, 1_785_553_200);
    assert_eq!(format_datetime_pt(midnight, -3), "01/08/2026 às 00:00");
}

// ---------------------------------------------------------------- payment

#[test]
fn credit_detected_from_token_balance_delta() {
    let tx = common::credit_tx(&common::recipient(), common::USDC, 150_000_000, common::NOW, "sig1");
    let credit = credit_in_tx(&tx, &common::recipient(), common::USDC).unwrap();
    assert_eq!(credit.amount_base, 150_000_000);
    assert_eq!(credit.signature, "sig1");
    assert_eq!(credit.block_time, Some(common::NOW));
    assert_eq!(credit.payer, Some(common::payer()));
}

#[test]
fn no_credit_for_wrong_mint_owner_or_failed_tx() {
    let tx = common::credit_tx(&common::recipient(), common::USDC, 150_000_000, common::NOW, "s");
    assert!(credit_in_tx(&tx, &common::destination(), common::USDC).is_none());
    assert!(credit_in_tx(&tx, &common::recipient(), "So11111111111111111111111111111111111111112").is_none());

    let mut failed = tx.clone();
    failed["meta"]["err"] = serde_json::json!({"InstructionError": [0, "Custom"]});
    assert!(credit_in_tx(&failed, &common::recipient(), common::USDC).is_none());

    assert!(credit_in_tx(&serde_json::Value::Null, &common::recipient(), common::USDC).is_none());
}

#[test]
fn status_decision_matrix() {
    let expires = 1_000;
    // full payment wins regardless of expiry
    assert_eq!(decide(100, 100, 500, expires), PayStatus::Paid);
    assert_eq!(decide(100, 150, 2_000, expires), PayStatus::Paid);
    // partial stays partial even after expiry (money is real)
    assert_eq!(decide(100, 50, 500, expires), PayStatus::Partial);
    assert_eq!(decide(100, 50, 2_000, expires), PayStatus::Partial);
    // nothing received: pending until expiry, expired after
    assert_eq!(decide(100, 0, 999, expires), PayStatus::Pending);
    assert_eq!(decide(100, 0, 1_001, expires), PayStatus::Expired);
}

// ---------------------------------------------------------------- tx wire

#[test]
fn shortvec_encoding_vectors() {
    let enc = |n: u16| {
        let mut v = Vec::new();
        shortvec(n, &mut v);
        v
    };
    assert_eq!(enc(0), vec![0]);
    assert_eq!(enc(1), vec![1]);
    assert_eq!(enc(127), vec![0x7f]);
    assert_eq!(enc(128), vec![0x80, 0x01]);
    assert_eq!(enc(300), vec![0xac, 0x02]);
}

#[test]
fn unsigned_transfer_checked_wire_layout() {
    let owner = common::recipient();
    let source = common::source_ata();
    let dest = common::dest_ata();
    let blockhash = common::blockhash();
    let wire = build_unsigned_transfer_checked(&SweepTransfer {
        owner: &owner,
        source: &source,
        destination: &dest,
        mint: common::USDC,
        token_program: clawpay::core::config::TOKEN_PROGRAM_ID,
        amount_base: 15_000_000,
        decimals: 6,
        blockhash: &blockhash,
    })
    .unwrap();

    // signature section: one empty 64-byte slot
    assert_eq!(wire[0], 1);
    assert!(wire[1..65].iter().all(|b| *b == 0));

    let msg = &wire[65..];
    // header: 1 signer, 0 readonly-signed, 2 readonly-unsigned
    assert_eq!(&msg[0..3], &[1, 0, 2]);
    // five account keys, owner first, program last
    assert_eq!(msg[3], 5);
    let keys = &msg[4..4 + 5 * 32];
    assert_eq!(&keys[0..32], [1u8; 32]); // owner == key(1)
    assert_eq!(
        &keys[4 * 32..5 * 32],
        bs58::decode(clawpay::core::config::TOKEN_PROGRAM_ID).into_vec().unwrap().as_slice()
    );
    // blockhash follows the key table
    let bh = &msg[4 + 160..4 + 192];
    assert_eq!(bh, [7u8; 32]); // blockhash == key(7)
    // one instruction: program index 4, accounts [source, mint, dest, owner]
    let instr = &msg[4 + 192..];
    assert_eq!(instr[0], 1);
    assert_eq!(instr[1], 4);
    assert_eq!(instr[2], 4);
    assert_eq!(&instr[3..7], &[1, 3, 2, 0]);
    // data: tag 12, amount LE, decimals
    assert_eq!(instr[7], 10);
    assert_eq!(instr[8], 12);
    assert_eq!(&instr[9..17], &15_000_000u64.to_le_bytes());
    assert_eq!(instr[17], 6);
    assert_eq!(instr.len(), 18); // nothing after the instruction
}

// ---------------------------------------------------------------- config

#[test]
fn empty_config_is_safe() {
    let cfg = Config::from_section(&HashMap::new()).unwrap();
    assert_eq!(cfg.recipient, None); // invoice creation will fail closed
    assert_eq!(cfg.yield_destination, None); // sweeps fail closed
    assert_eq!(cfg.max_sweep_pct, 0); // sweeps disabled
    assert!(!cfg.allow_recipient_override);
    assert_eq!(cfg.max_invoice_amount, "2000");
    assert_eq!(cfg.tokens.len(), 1);
    assert_eq!(cfg.tokens[0].symbol, "USDC");
    assert_eq!(cfg.tokens[0].decimals, 6);
    assert_eq!(cfg.utc_offset_hours, -3);
}

#[test]
fn sweep_pct_is_hard_ceilinged() {
    let mut section = HashMap::new();
    section.insert("max_sweep_pct".to_string(), "99".to_string());
    let cfg = Config::from_section(&section).unwrap();
    assert_eq!(cfg.max_sweep_pct, HARD_MAX_SWEEP_PCT);
}

#[test]
fn custom_token_list_parses_and_bad_ones_refuse() {
    let mut section = HashMap::new();
    section.insert(
        "allowed_tokens".to_string(),
        format!("USDC:{}:6,BRZ:{}:4", common::USDC, common::key(9)),
    );
    let cfg = Config::from_section(&section).unwrap();
    assert_eq!(cfg.tokens.len(), 2);
    assert_eq!(cfg.resolve_token(Some("brz")).unwrap().decimals, 4);
    assert!(cfg.resolve_token(Some("DOGE")).is_err());

    section.insert("allowed_tokens".to_string(), "USDC:notakey:6".to_string());
    assert!(Config::from_section(&section).is_err());
}

#[test]
fn invalid_config_pubkeys_fail_closed() {
    let mut section = HashMap::new();
    section.insert("recipient".to_string(), "not-base58!".to_string());
    assert!(Config::from_section(&section).is_err());

    let mut section = HashMap::new();
    section.insert("yield_destination".to_string(), "abc".to_string());
    assert!(Config::from_section(&section).is_err());
}

// ---------------------------------------------------------------- tickets

#[test]
fn hmac_sha256_matches_rfc4231_case_2() {
    let mac = clawpay::core::ticket::hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        mac.to_vec(),
        hex_decode("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
    );
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn hmac_sha256_matches_rfc4231_case_6_long_key() {
    let key = vec![0xaau8; 131];
    let mac = clawpay::core::ticket::hmac_sha256(
        &key,
        b"Test Using Larger Than Block-Size Key - Hash Key First",
    );
    assert_eq!(
        mac.to_vec(),
        hex_decode("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
    );
}

#[test]
fn ticket_roundtrip_and_tamper_detection() {
    use clawpay::core::ticket::{make, verify};
    let t = make("s3cret", &common::reference(), common::USDC, 150_000_000, 1_785_603_600, &common::recipient());
    assert!(verify("s3cret", &t, &common::reference(), common::USDC, 150_000_000, 1_785_603_600, &common::recipient()));
    // any altered field fails
    assert!(!verify("s3cret", &t, &common::reference(), common::USDC, 150_000_001, 1_785_603_600, &common::recipient()));
    assert!(!verify("s3cret", &t, &common::reference(), common::USDC, 150_000_000, 1_785_603_601, &common::recipient()));
    assert!(!verify("s3cret", &t, &common::destination(), common::USDC, 150_000_000, 1_785_603_600, &common::recipient()));
    assert!(!verify("other", &t, &common::reference(), common::USDC, 150_000_000, 1_785_603_600, &common::recipient()));
}
