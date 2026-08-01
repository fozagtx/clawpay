//! Error type carrying both a machine code and a natural Brazilian
//! Portuguese message ready for the agent to relay to the end user.
//!
//! Every failure path in ClawPay is fail-closed: a missing permission, an
//! unreachable RPC, or an exceeded limit refuses the operation with a clear
//! Portuguese explanation instead of proceeding with weaker guarantees.

use crate::core::config::TokenDef;

#[derive(Debug, Clone, PartialEq)]
pub struct ClawErr {
    /// Stable machine-readable code (English, snake_case).
    pub code: &'static str,
    /// Natural pt-BR message for the end user.
    pub message_pt: String,
    /// Short English detail for logs/operators.
    pub detail: String,
}

impl ClawErr {
    pub fn new(code: &'static str, message_pt: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { code, message_pt: message_pt.into(), detail: detail.into() }
    }

    pub fn config(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self::new(
            "config_error",
            "A configuração do ClawPay está incompleta ou inválida. \
             Peça ao operador para revisar a configuração do plugin.",
            detail,
        )
    }

    pub fn invalid_args(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self::new(
            "invalid_arguments",
            "Não entendi os dados da operação. Pode conferir o valor e tentar de novo?",
            detail,
        )
    }

    pub fn no_recipient() -> Self {
        Self::new(
            "recipient_not_configured",
            "Ainda não sei para qual carteira receber. \
             Peça ao operador para configurar o endereço de recebimento do ClawPay.",
            "config key `recipient` is not set and override is not allowed",
        )
    }

    pub fn recipient_override_forbidden() -> Self {
        Self::new(
            "recipient_override_forbidden",
            "Por segurança, só posso criar cobranças para a carteira configurada pelo operador.",
            "argument `recipient` given but allow_recipient_override is false",
        )
    }

    pub fn amount_too_high(max_fmt: &str, symbol: &str) -> Self {
        Self::new(
            "amount_above_limit",
            format!(
                "Não consigo criar cobrança acima de {max_fmt} {symbol}.\n\
                 Pode diminuir o valor?"
            ),
            "invoice amount exceeds max_invoice_amount",
        )
    }

    pub fn daily_limit_reached() -> Self {
        Self::new(
            "daily_limit_reached",
            "Você já atingiu o limite diário de recebimentos.\nTente novamente amanhã.",
            "daily_volume_cap reached",
        )
    }

    pub fn token_not_allowed(symbol: &str, allowed: &[TokenDef]) -> Self {
        let list = allowed
            .iter()
            .map(|t| t.symbol.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Self::new(
            "token_not_allowed",
            format!("Não trabalho com o token {symbol}. Tokens aceitos: {list}."),
            format!("token `{symbol}` not in allowed_tokens"),
        )
    }

    pub fn rpc(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self::new(
            "rpc_unavailable",
            "Não consegui consultar a rede Solana agora. \
             Vou tentar de novo em instantes. Pode repetir o comando daqui a pouco?",
            detail,
        )
    }

    pub fn scan_exhausted() -> Self {
        Self::new(
            "scan_window_exhausted",
            "Há movimentações demais nesta conta para eu verificar com segurança agora. \
             Tente novamente em instantes.",
            "signature pagination exhausted MAX_SCAN_PAGES while entries were still in the window",
        )
    }

    pub fn sweep_disabled() -> Self {
        Self::new(
            "sweep_disabled",
            "A reserva de rendimento não está ativada. \
             Peça ao operador para configurar `max_sweep_pct`, `yield_destination` \
             e `invoice_secret`.",
            "max_sweep_pct is 0, or yield_destination/invoice_secret missing",
        )
    }

    pub fn nonce_misconfigured(detail: &str) -> Self {
        Self::new(
            "nonce_misconfigured",
            "A conta de nonce da reserva não está configurada corretamente. \
             Peça ao operador para conferir a conta e a autoridade do nonce.",
            format!("nonce account problem: {detail}"),
        )
    }

    pub fn ticket_expired() -> Self {
        Self::new(
            "ticket_expired",
            "Essa cobrança expirou há mais de um dia, então não preparo mais \
             reservas para ela. Crie uma cobrança nova se precisar.",
            "sweep requested more than SWEEP_GRACE_SECS after invoice expiry",
        )
    }

    pub fn invalid_ticket() -> Self {
        Self::new(
            "invalid_ticket",
            "Esses dados não conferem com nenhuma cobrança emitida por mim, \
             então não vou preparar a reserva. Confira a referência e o valor da cobrança.",
            "ticket HMAC does not match the presented invoice fields",
        )
    }

    pub fn sweep_pct_too_high(max_pct: u8) -> Self {
        Self::new(
            "sweep_pct_above_limit",
            format!(
                "Só posso separar até {max_pct}% para a reserva de rendimento, \
                 conforme o limite configurado."
            ),
            "requested pct exceeds max_sweep_pct",
        )
    }

    pub fn sweep_daily_cap(remaining_fmt: &str, symbol: &str) -> Self {
        Self::new(
            "sweep_daily_cap_reached",
            format!(
                "O limite diário da reserva de rendimento foi atingido. \
                 Ainda cabem {remaining_fmt} {symbol} hoje; tente um valor menor ou aguarde amanhã."
            ),
            "daily_sweep_cap reached",
        )
    }

    pub fn not_paid_yet(ref_id: &str) -> Self {
        Self::new(
            "invoice_not_paid",
            format!(
                "A cobrança {ref_id} ainda não foi paga por completo, \
                 então não posso separar nada para a reserva ainda."
            ),
            "sweep requested before confirmed payment",
        )
    }

    pub fn destination_has_no_account(symbol: &str) -> Self {
        Self::new(
            "destination_token_account_missing",
            format!(
                "A carteira da reserva de rendimento ainda não tem uma conta de {symbol}. \
                 Peça ao operador para criá-la uma única vez."
            ),
            "yield destination has no token account for the mint",
        )
    }

    pub fn insufficient_balance(symbol: &str) -> Self {
        Self::new(
            "insufficient_balance",
            format!(
                "O saldo de {symbol} na carteira é menor que o valor da reserva. \
                 O dinheiro pode já ter sido movido."
            ),
            "source token account balance below sweep amount",
        )
    }
}
