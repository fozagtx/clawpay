//! Shared fixtures for the native test suite: deterministic pubkeys, a
//! programmable mock chain, and jsonParsed transaction builders.
//!
//! Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::collections::HashMap;

use clawpay::core::chain::ChainClient;
use serde_json::{json, Value};

/// Deterministic valid base58 32-byte pubkey per tag byte.
pub fn key(tag: u8) -> String {
    bs58::encode([tag; 32]).into_string()
}

pub fn recipient() -> String {
    key(1)
}
pub fn destination() -> String {
    key(2)
}
pub fn payer() -> String {
    key(3)
}
pub fn source_ata() -> String {
    key(4)
}
pub fn dest_ata() -> String {
    key(5)
}
pub fn reference() -> String {
    key(6)
}
pub fn blockhash() -> String {
    key(7)
}

pub const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const NOW: i64 = 1_785_600_000; // 2026-08-01 13:00 in Brasília (16:00 UTC)

pub fn base_config() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("recipient".to_string(), recipient());
    m
}

/// Closure-driven [`ChainClient`]; batches route through the same closure.
pub struct MockChain<F: Fn(&str, &Value) -> Result<Value, String>>(pub F);

impl<F: Fn(&str, &Value) -> Result<Value, String>> ChainClient for MockChain<F> {
    fn rpc(&self, _url: &str, method: &str, params: Value) -> Result<Value, String> {
        (self.0)(method, &params)
    }

    fn rpc_batch(&self, _url: &str, calls: &[(&str, Value)]) -> Result<Vec<Value>, String> {
        calls
            .iter()
            .map(|(method, params)| (self.0)(method, params))
            .collect()
    }
}

/// A chain that must never be consulted; panics if it is.
pub fn no_chain() -> MockChain<impl Fn(&str, &Value) -> Result<Value, String>> {
    MockChain(|method, _| panic!("unexpected chain call: {method}"))
}

/// A chain where every call fails, for fail-closed tests.
pub fn dead_chain() -> MockChain<impl Fn(&str, &Value) -> Result<Value, String>> {
    MockChain(|_, _| Err("connection refused".to_string()))
}

/// jsonParsed getTransaction result crediting `owner` with `delta` base units
/// of `mint` (already unwrapped from the RPC envelope).
pub fn credit_tx(owner: &str, mint: &str, delta: u64, block_time: i64, sig: &str) -> Value {
    json!({
        "blockTime": block_time,
        "slot": 100,
        "meta": {
            "err": null,
            "preTokenBalances": [
                {"accountIndex": 1, "mint": mint, "owner": owner,
                 "uiTokenAmount": {"amount": "0", "decimals": 6}},
                {"accountIndex": 2, "mint": mint, "owner": payer(),
                 "uiTokenAmount": {"amount": delta.to_string(), "decimals": 6}}
            ],
            "postTokenBalances": [
                {"accountIndex": 1, "mint": mint, "owner": owner,
                 "uiTokenAmount": {"amount": delta.to_string(), "decimals": 6}},
                {"accountIndex": 2, "mint": mint, "owner": payer(),
                 "uiTokenAmount": {"amount": "0", "decimals": 6}}
            ]
        },
        "transaction": {
            "signatures": [sig],
            "message": {"accountKeys": [
                {"pubkey": payer(), "signer": true, "writable": true},
                {"pubkey": owner, "signer": false, "writable": false}
            ]}
        }
    })
}

pub fn sig_entry(sig: &str, block_time: i64) -> Value {
    json!({"signature": sig, "err": null, "blockTime": block_time, "slot": 100})
}

pub fn token_account(pubkey: &str, balance: u64) -> Value {
    json!({
        "pubkey": pubkey,
        "account": {"data": {"parsed": {"info": {
            "tokenAmount": {"amount": balance.to_string(), "decimals": 6}
        }}}}
    })
}

pub fn run_output(result: &clawpay::core::api::ActionResult) -> Value {
    serde_json::from_str(&result.output).expect("tool output is JSON")
}
