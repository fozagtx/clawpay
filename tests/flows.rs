//! End-to-end action tests through `api::run` with a mocked chain: the same
//! JSON-in/JSON-out surface the ZeroClaw host drives, including every
//! fail-closed path and the Portuguese copy.

mod common;

use common::*;
use serde_json::{json, Value};

use clawpay::core::api::run;

const ENTROPY: [u8; 32] = [9u8; 32];

fn args(mut body: Value, config: &std::collections::HashMap<String, String>) -> String {
    body["__config"] = json!(config);
    body.to_string()
}

const SECRET: &str = "test-invoice-secret-0123456789";

fn sweep_config() -> std::collections::HashMap<String, String> {
    let mut cfg = base_config();
    cfg.insert("yield_destination".to_string(), destination());
    cfg.insert("max_sweep_pct".to_string(), "20".to_string());
    cfg.insert("daily_sweep_cap".to_string(), "500".to_string());
    cfg.insert("invoice_secret".to_string(), SECRET.to_string());
    cfg
}

fn valid_ticket() -> String {
    clawpay::core::ticket::make(SECRET, &reference(), USDC, 150_000_000, NOW + 3600, &recipient())
}

// ------------------------------------------------------------------ create

#[test]
fn create_invoice_happy_path() {
    let a = args(
        json!({"action": "create_invoice", "amount": "150", "description": "Almoço"}),
        &base_config(),
    );
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
    let out = run_output(&result);

    assert_eq!(out["status"], "created");
    assert_eq!(out["token"], "USDC");
    assert_eq!(out["amount"], "150");
    assert_eq!(out["expires_at"], json!(NOW + 3600));
    let url = out["url"].as_str().unwrap();
    assert!(url.starts_with(&format!("solana:{}?amount=150&spl-token={}", recipient(), USDC)));
    assert!(url.contains(&format!("reference={}", out["reference"].as_str().unwrap())));
    assert!(url.contains("message=Almo%C3%A7o"));
    assert!(url.contains("label=ClawPay"));
    let ref_id = out["reference_id"].as_str().unwrap();
    assert!(ref_id.starts_with("CP-") && ref_id.len() == 8);
    assert!(url.contains(&format!("memo={ref_id}")));
    assert_eq!(out["qr_content"], out["url"]);

    let msg = out["message_pt"].as_str().unwrap();
    assert!(msg.contains("Cobrança de 150,00 USDC criada"));
    assert!(msg.contains(&format!("Referência: {ref_id}")));
    assert!(msg.contains("Válida até"));
}

#[test]
fn create_invoice_expiry_and_comma_amount() {
    let a = args(
        json!({"action": "create_invoice", "amount": "1.234,56", "expiry_minutes": 10}),
        &base_config(),
    );
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(result.success);
    let out = run_output(&result);
    assert_eq!(out["amount"], "1234.56");
    assert_eq!(out["amount_formatted"], "1.234,56");
    assert_eq!(out["expires_at"], json!(NOW + 600));
}

#[test]
fn create_invoice_distinct_entropy_distinct_reference() {
    let a = args(json!({"action": "create_invoice", "amount": "10"}), &base_config());
    let first = run_output(&run(&a, &no_chain(), [1u8; 32], NOW));
    let second = run_output(&run(&a, &no_chain(), [2u8; 32], NOW));
    assert_ne!(first["reference"], second["reference"]);
    assert_ne!(first["reference_id"], second["reference_id"]);
}

#[test]
fn create_invoice_above_max_fails_closed() {
    let a = args(json!({"action": "create_invoice", "amount": "2000,01"}), &base_config());
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    let out = run_output(&result);
    assert_eq!(out["code"], "amount_above_limit");
    assert!(out["message_pt"]
        .as_str()
        .unwrap()
        .contains("Não consigo criar cobrança acima de 2.000,00 USDC"));
}

#[test]
fn create_invoice_recipient_override_refused() {
    let a = args(
        json!({"action": "create_invoice", "amount": "10", "recipient": destination()}),
        &base_config(),
    );
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "recipient_override_forbidden");
}

#[test]
fn create_invoice_without_recipient_config_fails_closed() {
    let a = args(
        json!({"action": "create_invoice", "amount": "10"}),
        &std::collections::HashMap::new(),
    );
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "recipient_not_configured");
}

#[test]
fn create_invoice_unknown_token_refused() {
    let a = args(
        json!({"action": "create_invoice", "amount": "10", "token": "DOGE"}),
        &base_config(),
    );
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "token_not_allowed");
}

// -------------------------------------------------- create + daily volume

fn volume_chain(
    todays_base: u64,
) -> MockChain<impl Fn(&str, &Value) -> Result<Value, String>> {
    let recip = recipient();
    let ata = source_ata();
    MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getTokenAccountsByOwner" if addr == recip => {
                Ok(json!({"value": [token_account(&ata, 0)]}))
            }
            "getSignaturesForAddress" if addr == ata => {
                Ok(json!([sig_entry("daysig", NOW - 120)]))
            }
            "getTransaction" => Ok(credit_tx(&recip, USDC, todays_base, NOW - 120, "daysig")),
            other => Err(format!("unmocked {other}")),
        }
    })
}

#[test]
fn daily_volume_cap_blocks_creation() {
    let mut cfg = base_config();
    cfg.insert("daily_volume_cap".to_string(), "2000".to_string());
    let a = args(json!({"action": "create_invoice", "amount": "150"}), &cfg);

    // 1.900 already received today + 150 > 2.000 -> refuse
    let result = run(&a, &volume_chain(1_900_000_000), ENTROPY, NOW);
    assert!(!result.success);
    let out = run_output(&result);
    assert_eq!(out["code"], "daily_limit_reached");
    assert!(out["message_pt"].as_str().unwrap().contains("limite diário"));

    // 1.000 received today + 150 fits
    let result = run(&a, &volume_chain(1_000_000_000), ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
}

#[test]
fn daily_volume_cap_fails_closed_when_rpc_is_down() {
    let mut cfg = base_config();
    cfg.insert("daily_volume_cap".to_string(), "2000".to_string());
    let a = args(json!({"action": "create_invoice", "amount": "150"}), &cfg);
    let result = run(&a, &dead_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "rpc_unavailable");
}

// ----------------------------------------------------------- check_payment

fn check_args(expected: &str, expires_at: i64) -> Value {
    json!({
        "action": "check_payment",
        "reference": reference(),
        "expected_amount": expected,
        "expires_at": expires_at,
    })
}

/// Chain with `credits` (amount, blocktime) paid against the reference.
fn paid_chain(
    credits: Vec<(u64, i64)>,
) -> MockChain<impl Fn(&str, &Value) -> Result<Value, String>> {
    let reference = reference();
    let recip = recipient();
    MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getSignaturesForAddress" if addr == reference => Ok(Value::Array(
                credits
                    .iter()
                    .enumerate()
                    .map(|(i, (_, bt))| sig_entry(&format!("sig{i}"), *bt))
                    .collect(),
            )),
            "getTransaction" => {
                let sig = addr;
                let idx: usize = sig.trim_start_matches("sig").parse().unwrap();
                let (amount, bt) = credits[idx];
                Ok(credit_tx(&recip, USDC, amount, bt, sig))
            }
            other => Err(format!("unmocked {other}")),
        }
    })
}

#[test]
fn check_payment_paid() {
    let a = args(check_args("150", NOW + 3600), &base_config());
    let result = run(&a, &paid_chain(vec![(150_000_000, NOW - 60)]), ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
    let out = run_output(&result);
    assert_eq!(out["status"], "paid");
    assert_eq!(out["received_amount"], "150");
    assert_eq!(out["paid_at"], json!(NOW - 60));
    assert_eq!(out["payer"], json!(payer()));
    let msg = out["message_pt"].as_str().unwrap();
    assert!(msg.contains("Pagamento confirmado!"));
    assert!(msg.contains("Você recebeu 150,00 USDC"));
    assert!(msg.contains("Obrigado!"));
}

#[test]
fn check_payment_pending_and_expired() {
    let empty = || paid_chain(vec![]);

    let a = args(check_args("150", NOW + 3600), &base_config());
    let out = run_output(&run(&a, &empty(), ENTROPY, NOW));
    assert_eq!(out["status"], "pending");
    assert!(out["message_pt"].as_str().unwrap().contains("Ainda não recebi o pagamento"));

    let a = args(check_args("150", NOW - 10), &base_config());
    let out = run_output(&run(&a, &empty(), ENTROPY, NOW));
    assert_eq!(out["status"], "expired");
    assert!(out["message_pt"].as_str().unwrap().contains("expirou sem pagamento"));
}

#[test]
fn check_payment_partial_sums_multiple_credits() {
    let a = args(check_args("150", NOW + 3600), &base_config());
    let chain = paid_chain(vec![(60_000_000, NOW - 300), (40_000_000, NOW - 200)]);
    let out = run_output(&run(&a, &chain, ENTROPY, NOW));
    assert_eq!(out["status"], "partial");
    assert_eq!(out["received_amount"], "100");
    let msg = out["message_pt"].as_str().unwrap();
    assert!(msg.contains("Recebi apenas 100,00 USDC da cobrança de 150,00 USDC"));
    assert!(msg.contains("Faltam 50,00 USDC"));
}

#[test]
fn check_payment_rpc_down_fails_closed() {
    let a = args(check_args("150", NOW + 3600), &base_config());
    let result = run(&a, &dead_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "rpc_unavailable");
}

// -------------------------------------------------------------- sweep_yield

fn sweep_args(pct: u8) -> Value {
    json!({
        "action": "sweep_yield",
        "reference": reference(),
        "expected_amount": "150",
        "expires_at": NOW + 3600,
        "pct": pct,
        "ticket": valid_ticket(),
    })
}

/// Fully wired sweep chain: payment of 150 USDC confirmed, `dest_today`
/// already received by the destination today, `source_balance` in the
/// merchant's token account.
fn sweep_chain(
    dest_today: Option<(u64, i64)>,
    source_balance: u64,
    dest_has_account: bool,
) -> MockChain<impl Fn(&str, &Value) -> Result<Value, String>> {
    let reference = reference();
    let recip = recipient();
    let dest = destination();
    let s_ata = source_ata();
    let d_ata = dest_ata();
    let bh = blockhash();
    MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getSignaturesForAddress" if addr == reference => {
                Ok(json!([sig_entry("paysig", NOW - 600)]))
            }
            "getSignaturesForAddress" if addr == d_ata => match dest_today {
                Some((_, bt)) => Ok(json!([sig_entry("destsig", bt)])),
                None => Ok(json!([])),
            },
            "getTransaction" if addr == "paysig" => {
                Ok(credit_tx(&recip, USDC, 150_000_000, NOW - 600, "paysig"))
            }
            "getTransaction" if addr == "destsig" => {
                let (amount, bt) = dest_today.unwrap();
                Ok(credit_tx(&dest, USDC, amount, bt, "destsig"))
            }
            "getTokenAccountsByOwner" if addr == recip => {
                Ok(json!({"value": [token_account(&s_ata, source_balance)]}))
            }
            "getTokenAccountsByOwner" if addr == dest => {
                if dest_has_account {
                    Ok(json!({"value": [token_account(&d_ata, 0)]}))
                } else {
                    Ok(json!({"value": []}))
                }
            }
            "getLatestBlockhash" => Ok(json!({"value": {"blockhash": bh}})),
            other => Err(format!("unmocked {other}")),
        }
    })
}

#[test]
fn sweep_happy_path_builds_unsigned_tx() {
    let a = args(sweep_args(10), &sweep_config());
    let chain = sweep_chain(None, 150_000_000, true);
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
    let out = run_output(&result);

    assert_eq!(out["status"], "sweep_ready");
    assert_eq!(out["sweep_amount"], "15");
    assert_eq!(out["pct"], 10);
    assert_eq!(out["destination_owner"], json!(destination()));
    assert_eq!(out["source_token_account"], json!(source_ata()));

    use base64::Engine as _;
    let wire = base64::engine::general_purpose::STANDARD
        .decode(out["unsigned_tx_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(wire[0], 1);
    assert!(wire[1..65].iter().all(|b| *b == 0), "signature slot must be empty");

    let msg = out["message_pt"].as_str().unwrap();
    assert!(msg.contains("Separei 10% (15,00 USDC)"));
    assert!(msg.contains("reserva de rendimento"));
}

#[test]
fn sweep_refused_when_disabled() {
    // No yield_destination / max_sweep_pct in config.
    let a = args(sweep_args(10), &base_config());
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "sweep_disabled");
}

#[test]
fn sweep_pct_above_operator_cap_refused() {
    let a = args(sweep_args(21), &sweep_config());
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    let out = run_output(&result);
    assert_eq!(out["code"], "sweep_pct_above_limit");
    assert!(out["message_pt"].as_str().unwrap().contains("até 20%"));
}

#[test]
fn sweep_before_payment_refused() {
    let reference = reference();
    let chain = MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getSignaturesForAddress" if addr == reference => Ok(json!([])),
            other => Err(format!("unmocked {other}")),
        }
    });
    let a = args(sweep_args(10), &sweep_config());
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(!result.success);
    let out = run_output(&result);
    assert_eq!(out["code"], "invoice_not_paid");
    assert!(out["message_pt"].as_str().unwrap().contains("ainda não foi paga"));
}

#[test]
fn sweep_daily_cap_enforced_from_chain() {
    // Destination already received 495 USDC today; sweeping 15 would breach
    // the 500 cap -> refuse and tell how much still fits.
    let a = args(sweep_args(10), &sweep_config());
    let chain = sweep_chain(Some((495_000_000, NOW - 400)), 150_000_000, true);
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(!result.success);
    let out = run_output(&result);
    assert_eq!(out["code"], "sweep_daily_cap_reached");
    assert!(out["message_pt"].as_str().unwrap().contains("5,00 USDC"));
}

#[test]
fn sweep_daily_cap_ignores_yesterday() {
    // 495 received BEFORE local midnight must not count.
    let yesterday = clawpay::core::time::local_midnight(NOW, -3) - 100;
    let a = args(sweep_args(10), &sweep_config());
    let chain = sweep_chain(Some((495_000_000, yesterday)), 150_000_000, true);
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
}

#[test]
fn sweep_insufficient_source_balance_refused() {
    let a = args(sweep_args(10), &sweep_config());
    let chain = sweep_chain(None, 10_000_000, true); // only 10 USDC left
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "insufficient_balance");
}

#[test]
fn sweep_missing_destination_account_refused() {
    let a = args(sweep_args(10), &sweep_config());
    let chain = sweep_chain(None, 150_000_000, false);
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "destination_token_account_missing");
}

#[test]
fn sweep_rpc_down_fails_closed() {
    let a = args(sweep_args(10), &sweep_config());
    let result = run(&a, &dead_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "rpc_unavailable");
}

// ------------------------------------------------------------- dispatch

#[test]
fn unknown_action_and_bad_json_refused() {
    let a = args(json!({"action": "transfer_everything"}), &base_config());
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "invalid_arguments");

    let result = run("not json", &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "invalid_arguments");
}

#[test]
fn model_cannot_smuggle_config() {
    // Even if a `__config` object appears in the arguments (the host strips
    // it in production), it only configures; it cannot enable sweeps beyond
    // the hard ceiling.
    let mut cfg = sweep_config();
    cfg.insert("max_sweep_pct".to_string(), "90".to_string());
    let a = args(sweep_args(30), &cfg);
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "sweep_pct_above_limit");
}

// ------------------------------------------- fail-closed hardening regressions

/// A per-item batch failure (Null slot) must refuse the lookup, never count
/// as "paid nothing".
#[test]
fn per_item_batch_failure_refuses_instead_of_zero() {
    let reference = reference();
    let chain = MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getSignaturesForAddress" if addr == reference => {
                Ok(json!([sig_entry("paysig", NOW - 60)]))
            }
            "getTransaction" => Ok(Value::Null), // simulated per-item RPC error
            other => Err(format!("unmocked {other}")),
        }
    });
    let a = args(check_args("150", NOW + 3600), &base_config());
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "rpc_unavailable");
}

/// Daily-cap scans must paginate: credits beyond the first signature page
/// still count against the cap.
#[test]
fn daily_volume_cap_sees_past_first_signature_page() {
    let mut cfg = base_config();
    cfg.insert("daily_volume_cap".to_string(), "2000".to_string());
    cfg.insert("scan_limit".to_string(), "2".to_string());
    let recip = recipient();
    let ata = source_ata();
    // Page 1: two zero-credit txs today (full page). Page 2: the 1.900 credit.
    let chain = MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getTokenAccountsByOwner" if addr == recip => {
                Ok(json!({"value": [token_account(&ata, 0)]}))
            }
            "getSignaturesForAddress" if addr == ata => {
                let before = params
                    .get(1)
                    .and_then(|o| o.get("before"))
                    .and_then(Value::as_str);
                match before {
                    None => Ok(json!([sig_entry("s1", NOW - 10), sig_entry("s2", NOW - 20)])),
                    Some("s2") => Ok(json!([sig_entry("s3", NOW - 30)])),
                    other => Err(format!("unexpected before {other:?}")),
                }
            }
            "getTransaction" if addr == "s3" => {
                Ok(credit_tx(&recip, USDC, 1_900_000_000, NOW - 30, "s3"))
            }
            "getTransaction" => Ok(credit_tx(&recip, USDC, 0, NOW - 10, addr)),
            other => Err(format!("unmocked {other}")),
        }
    });
    let a = args(json!({"action": "create_invoice", "amount": "150"}), &cfg);
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(!result.success, "1.900 on page 2 must still trip the 2.000 cap");
    assert_eq!(run_output(&result)["code"], "daily_limit_reached");
}

/// When pagination never leaves the time window, the scan fails closed
/// instead of undercounting.
#[test]
fn exhausted_scan_window_refuses() {
    let mut cfg = base_config();
    cfg.insert("daily_volume_cap".to_string(), "2000".to_string());
    cfg.insert("scan_limit".to_string(), "1".to_string());
    let recip = recipient();
    let ata = source_ata();
    let chain = MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getTokenAccountsByOwner" if addr == recip => {
                Ok(json!({"value": [token_account(&ata, 0)]}))
            }
            // Always a full page, always inside today's window.
            "getSignaturesForAddress" if addr == ata => {
                let before = params
                    .get(1)
                    .and_then(|o| o.get("before"))
                    .and_then(Value::as_str)
                    .unwrap_or("s0");
                Ok(json!([sig_entry(&format!("{before}x"), NOW - 60)]))
            }
            other => Err(format!("unmocked {other}")),
        }
    });
    let a = args(json!({"action": "create_invoice", "amount": "150"}), &cfg);
    let result = run(&a, &chain, ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "scan_window_exhausted");
}

/// Model-supplied expiry is clamped to one week, never overflowing.
#[test]
fn expiry_minutes_is_clamped() {
    let a = args(
        json!({"action": "create_invoice", "amount": "10", "expiry_minutes": u64::MAX}),
        &base_config(),
    );
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
    let out = run_output(&result);
    assert_eq!(out["expires_at"], json!(NOW + 7 * 24 * 3600));
}

/// A self-sweep configuration is refused at config time.
#[test]
fn yield_destination_equal_to_recipient_refused() {
    let mut cfg = base_config();
    cfg.insert("yield_destination".to_string(), recipient());
    cfg.insert("max_sweep_pct".to_string(), "10".to_string());
    let a = args(json!({"action": "create_invoice", "amount": "10"}), &cfg);
    let result = run(&a, &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "config_error");
}


// ---------------------------------------------------------- invoice tickets

/// A ticket for different invoice fields (or garbage) must refuse the sweep.
#[test]
fn sweep_with_forged_or_mismatched_ticket_refused() {
    let mut a: Value = sweep_args(10);
    a["ticket"] = json!(clawpay::core::ticket::make(
        SECRET, &reference(), USDC, 100_000_000, NOW + 3600, &recipient()
    ));
    let result = run(&args(a, &sweep_config()), &no_chain(), ENTROPY, NOW);
    assert!(!result.success, "ticket issued for 100 must not authorize a 150 sweep");
    assert_eq!(run_output(&result)["code"], "invalid_ticket");

    let mut a: Value = sweep_args(10);
    a["ticket"] = json!("definitely-not-a-ticket");
    let result = run(&args(a, &sweep_config()), &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "invalid_ticket");
}

/// Without a ticket the sweep refuses before touching the chain.
#[test]
fn sweep_without_ticket_refused() {
    let mut a: Value = sweep_args(10);
    a.as_object_mut().unwrap().remove("ticket");
    let result = run(&args(a, &sweep_config()), &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "invalid_arguments");
}

/// Sweeps stay disabled when the operator has not configured a secret.
#[test]
fn sweep_without_secret_config_disabled() {
    let mut cfg = sweep_config();
    cfg.remove("invoice_secret");
    let result = run(&args(sweep_args(10), &cfg), &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "sweep_disabled");
}

/// The ticket returned by create_invoice authorizes the matching sweep.
#[test]
fn create_then_sweep_roundtrip_with_ticket() {
    let cfg = sweep_config();
    let created = run(
        &args(json!({"action": "create_invoice", "amount": "150"}), &cfg),
        &no_chain(),
        [6u8; 32], // reference() == bs58([6; 32])
        NOW,
    );
    assert!(created.success);
    let out = run_output(&created);
    assert_eq!(out["reference"], json!(reference()));
    let ticket = out["ticket"].as_str().expect("ticket issued when secret configured").to_string();

    let sweep = json!({
        "action": "sweep_yield",
        "reference": out["reference"],
        "expected_amount": out["amount"],
        "expires_at": out["expires_at"],
        "pct": 10,
        "ticket": ticket,
    });
    let chain = sweep_chain(None, 150_000_000, true);
    let result = run(&args(sweep, &cfg), &chain, ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
    assert_eq!(run_output(&result)["status"], "sweep_ready");
}

/// No secret configured: create_invoice still works, just without a ticket.
#[test]
fn create_without_secret_has_no_ticket() {
    let a = args(json!({"action": "create_invoice", "amount": "10"}), &base_config());
    let out = run_output(&run(&a, &no_chain(), ENTROPY, NOW));
    assert!(out["ticket"].is_null());
}


/// A ticket ages out one day after the invoice expires: no indefinite
/// re-sweeping of an old paid invoice.
#[test]
fn sweep_long_after_expiry_refused() {
    let expires = NOW - 2 * 24 * 3600;
    let a = json!({
        "action": "sweep_yield",
        "reference": reference(),
        "expected_amount": "150",
        "expires_at": expires,
        "pct": 10,
        "ticket": clawpay::core::ticket::make(SECRET, &reference(), USDC, 150_000_000, expires, &recipient()),
    });
    let result = run(&args(a, &sweep_config()), &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "ticket_expired");
}

/// Brute-forceable secrets are refused at config time.
#[test]
fn short_invoice_secret_refused() {
    let mut cfg = sweep_config();
    cfg.insert("invoice_secret".to_string(), "short".to_string());
    let result = run(&args(sweep_args(10), &cfg), &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "config_error");
}

/// Duplicate symbols or mints in allowed_tokens are ambiguous and refused.
#[test]
fn duplicate_allowed_tokens_refused() {
    let mut cfg = base_config();
    cfg.insert(
        "allowed_tokens".to_string(),
        format!("USDC:{USDC}:6,USDX:{USDC}:9"),
    );
    let result = run(
        &args(json!({"action": "create_invoice", "amount": "10"}), &cfg),
        &no_chain(),
        ENTROPY,
        NOW,
    );
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "config_error");
}


// ------------------------------------------------------------ durable nonce

fn nonce_chain(
    authority: String,
) -> MockChain<impl Fn(&str, &Value) -> Result<Value, String>> {
    let reference = reference();
    let recip = recipient();
    let dest = destination();
    let s_ata = source_ata();
    let d_ata = dest_ata();
    let nonce_account = key(8);
    MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getSignaturesForAddress" if addr == reference => {
                Ok(json!([sig_entry("paysig", NOW - 600)]))
            }
            "getSignaturesForAddress" if addr == d_ata => Ok(json!([])),
            "getTransaction" if addr == "paysig" => {
                Ok(credit_tx(&recip, USDC, 150_000_000, NOW - 600, "paysig"))
            }
            "getTokenAccountsByOwner" if addr == recip => {
                Ok(json!({"value": [token_account(&s_ata, 150_000_000)]}))
            }
            "getTokenAccountsByOwner" if addr == dest => {
                Ok(json!({"value": [token_account(&d_ata, 0)]}))
            }
            "getAccountInfo" if addr == nonce_account => Ok(json!({
                "value": {"data": {"program": "nonce", "parsed": {"type": "initialized", "info": {
                    "authority": authority,
                    "blockhash": blockhash(),
                }}}}
            })),
            other => Err(format!("unmocked {other}")),
        }
    })
}

/// With a nonce account configured, the sweep builds on the stored nonce
/// blockhash and includes the advance instruction (8-key account table).
#[test]
fn sweep_with_durable_nonce() {
    let mut cfg = sweep_config();
    cfg.insert("nonce_account".to_string(), key(8));
    let result = run(&args(sweep_args(10), &cfg), &nonce_chain(recipient()), ENTROPY, NOW);
    assert!(result.success, "error: {:?}", result.error);
    let out = run_output(&result);
    assert_eq!(out["status"], "sweep_ready");
    assert!(out["signing_note"].as_str().unwrap().contains("nonce durável"));

    use base64::Engine as _;
    let wire = base64::engine::general_purpose::STANDARD
        .decode(out["unsigned_tx_base64"].as_str().unwrap())
        .unwrap();
    let msg = &wire[65..];
    assert_eq!(&msg[0..3], &[1, 0, 4]);
    assert_eq!(msg[3], 8);
}

/// A nonce account whose authority is not the recipient wallet refuses.
#[test]
fn sweep_with_foreign_nonce_authority_refused() {
    let mut cfg = sweep_config();
    cfg.insert("nonce_account".to_string(), key(8));
    let result = run(&args(sweep_args(10), &cfg), &nonce_chain(payer()), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "nonce_misconfigured");
}


/// An account that is not an initialized nonce refuses, even with
/// authority-shaped and blockhash-shaped fields present.
#[test]
fn sweep_with_non_nonce_account_refused() {
    let mut cfg = sweep_config();
    cfg.insert("nonce_account".to_string(), key(8));
    let nonce_account = key(8);
    let chain = MockChain(move |method, params| {
        let addr = params.get(0).and_then(Value::as_str).unwrap_or("");
        match method {
            "getSignaturesForAddress" => Ok(json!([sig_entry("paysig", NOW - 600)])),
            "getTransaction" => Ok(credit_tx(&recipient(), USDC, 150_000_000, NOW - 600, "paysig")),
            "getTokenAccountsByOwner" if addr == recipient() => {
                Ok(json!({"value": [token_account(&source_ata(), 150_000_000)]}))
            }
            "getTokenAccountsByOwner" => Ok(json!({"value": [token_account(&dest_ata(), 0)]})),
            "getAccountInfo" if addr == nonce_account => Ok(json!({
                "value": {"data": {"program": "spl-token", "parsed": {"type": "account", "info": {
                    "authority": recipient(),
                    "blockhash": blockhash(),
                }}}}
            })),
            other => Err(format!("unmocked {other}")),
        }
    });
    let result = run(&args(sweep_args(10), &cfg), &chain, ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "nonce_misconfigured");
}

/// The nonce account must be its own account, not another configured wallet.
#[test]
fn nonce_account_colliding_with_recipient_refused() {
    let mut cfg = sweep_config();
    cfg.insert("nonce_account".to_string(), recipient());
    let result = run(&args(sweep_args(10), &cfg), &no_chain(), ENTROPY, NOW);
    assert!(!result.success);
    assert_eq!(run_output(&result)["code"], "config_error");
}
