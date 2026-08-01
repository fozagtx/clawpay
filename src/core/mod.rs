//! ClawPay's pure core: no wasm, no I/O, no globals.
//!
//! Everything here compiles and unit-tests natively (`cargo test`). The wasm
//! component in `lib.rs` is a thin shim that injects the real chain client,
//! entropy, and clock.

pub mod api;
pub mod chain;
pub mod config;
pub mod error;
pub mod invoice;
pub mod money;
pub mod msgs;
pub mod payment;
pub mod ticket;
pub mod time;
pub mod tx;
