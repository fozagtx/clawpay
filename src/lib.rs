//! ClawPay, a ZeroClaw WIT tool plugin: Solana Pay stablecoin invoices for
//! Brazilian informal workers and small merchants.
//!
//! One tool, three actions:
//! - `create_invoice`: Solana Pay link + QR content + unique reference
//! - `check_payment`:  read-only on-chain detection and pt-BR confirmation
//!   data
//! - `sweep_yield`:    hard-capped, chain-verified, UNSIGNED transfer to a
//!   pre-configured yield wallet
//!
//! Custody: the plugin is read-only plus build-unsigned-transaction. It never
//! holds key material and has no signing path; every cap (per-invoice max,
//! daily volume, sweep percentage, daily sweep cap) is enforced inside the
//! plugin from operator config and on-chain facts, failing closed.
//!
//! The payments core lives in [`core`] with no wasm dependency, so it
//! compiles and tests on the host with a plain `cargo test`; the component
//! reuses the exact same logic through this shim.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release

pub mod core;

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use serde_json::Value;

    use crate::core::api;
    use crate::core::chain::ChainClient;
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };

    struct ClawPay;

    const PLUGIN_NAME: &str = "clawpay";
    const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
    const TOOL_NAME: &str = "clawpay";

    /// Solana JSON-RPC over the granted `wasi:http` surface. Without the
    /// `http_client` permission every call errors, which the core maps to a
    /// fail-closed Portuguese refusal.
    struct WakiChain;

    impl WakiChain {
        fn post(&self, url: &str, body: Value) -> Result<Value, String> {
            let resp = waki::Client::new()
                .post(url)
                .header("Content-Type", "application/json")
                .body(body.to_string().into_bytes())
                .connect_timeout(std::time::Duration::from_secs(10))
                .send()
                .map_err(|e| format!("rpc request failed: {e}"))?;
            let status = resp.status_code();
            let bytes = resp.body().map_err(|e| format!("rpc body read failed: {e}"))?;
            if !(200..300).contains(&status) {
                return Err(format!("rpc http status {status}"));
            }
            serde_json::from_slice(&bytes).map_err(|e| format!("rpc bad JSON: {e}"))
        }

        fn unwrap_result(entry: &Value) -> Result<Value, String> {
            if let Some(err) = entry.get("error").filter(|e| !e.is_null()) {
                return Err(format!("rpc error: {err}"));
            }
            Ok(entry.get("result").cloned().unwrap_or(Value::Null))
        }
    }

    impl ChainClient for WakiChain {
        fn rpc(&self, url: &str, method: &str, params: Value) -> Result<Value, String> {
            let body = serde_json::json!({
                "jsonrpc": "2.0", "id": 0, "method": method, "params": params
            });
            Self::unwrap_result(&self.post(url, body)?)
        }

        fn rpc_batch(&self, url: &str, calls: &[(&str, Value)]) -> Result<Vec<Value>, String> {
            let body: Value = calls
                .iter()
                .enumerate()
                .map(|(id, (method, params))| {
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id, "method": method, "params": params
                    })
                })
                .collect::<Vec<_>>()
                .into();
            let response = self.post(url, body)?;
            let entries = response
                .as_array()
                .ok_or_else(|| "rpc batch response is not an array".to_string())?;
            // Responses may arrive in any order; slot them by id, Null for
            // per-item errors so one bad transaction never sinks the batch.
            let mut out = vec![Value::Null; calls.len()];
            for entry in entries {
                let Some(id) = entry.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                if let Some(slot) = out.get_mut(id as usize) {
                    *slot = Self::unwrap_result(entry).unwrap_or(Value::Null);
                }
            }
            Ok(out)
        }
    }

    fn entropy() -> [u8; 32] {
        let mut buf = [0u8; 32];
        if getrandom::fill(&mut buf).is_ok() {
            return buf;
        }
        // wasi:random should always be present; if it is not, degrade to a
        // time-seeded hash rather than a constant reference.
        use sha2::{Digest, Sha256};
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Sha256::digest(nanos.to_le_bytes()).into()
    }

    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn emit(action: PluginAction, outcome: PluginOutcome, message: &str, code: Option<&str>) {
        let attrs = code.map(|c| format!("{{\"code\":\"{c}\"}}"));
        log_record(
            LogLevel::Info,
            &PluginEvent {
                function_name: "clawpay::tool::execute".to_string(),
                action,
                outcome: Some(outcome),
                duration_ms: None,
                attrs,
                message: message.to_string(),
            },
        );
    }

    impl PluginInfo for ClawPay {
        fn plugin_name() -> String {
            PLUGIN_NAME.to_string()
        }

        fn plugin_version() -> String {
            PLUGIN_VERSION.to_string()
        }
    }

    impl Tool for ClawPay {
        fn name() -> String {
            TOOL_NAME.to_string()
        }

        fn description() -> String {
            "Terminal de pagamentos Solana Pay para trabalhadores e comerciantes \
             brasileiros. Cria cobranças em stablecoin (USDC por padrão) com link e \
             QR, verifica na blockchain se uma cobrança foi paga (paga/pendente/\
             parcial/expirada) e, após pagamento confirmado, prepara uma transação \
             NÃO ASSINADA para separar uma porcentagem limitada em uma reserva de \
             rendimento pré-configurada. Todos os limites são impostos pelo plugin; \
             ele nunca assina transações. Use check_payment sempre que o usuário \
             perguntar se já pagaram."
                .to_string()
        }

        fn parameters_schema() -> String {
            api::parameters_schema()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            emit(PluginAction::Start, PluginOutcome::Success, "executing clawpay action", None);
            let result = api::run(&args, &WakiChain, entropy(), now_unix());
            if result.success {
                emit(PluginAction::Complete, PluginOutcome::Success, "clawpay action complete", None);
            } else {
                emit(
                    PluginAction::Fail,
                    PluginOutcome::Failure,
                    "clawpay action refused or failed",
                    result.error.as_deref().and_then(|e| e.split(':').next()),
                );
            }
            Ok(ToolResult {
                success: result.success,
                output: result.output,
                error: result.error,
            })
        }
    }

    export!(ClawPay);
}
