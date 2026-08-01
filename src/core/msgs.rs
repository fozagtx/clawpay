//! Natural Brazilian Portuguese message templates.
//!
//! Amounts are denominated in the invoice token (USDC by default) and
//! formatted Brazilian-style (`1.234,56`). Converting to and displaying BRL
//! is the agent's job; the plugin never invents an exchange rate.

pub fn created(amount: &str, symbol: &str, url: &str, ref_id: &str, valid_until: &str) -> String {
    format!(
        "Pronto! Cobrança de {amount} {symbol} criada.\n\
         Link de pagamento: {url}\n\
         Referência: {ref_id}\n\
         Válida até {valid_until}."
    )
}

pub fn paid(amount: &str, symbol: &str, ref_id: &str, when: &str) -> String {
    format!(
        "Pagamento confirmado!\n\
         Você recebeu {amount} {symbol}.\n\
         Referência: {ref_id}\n\
         Data: {when}.\n\
         Obrigado!"
    )
}

pub fn pending(ref_id: &str) -> String {
    format!(
        "Ainda não recebi o pagamento da cobrança {ref_id}.\n\
         Quer que eu continue monitorando?"
    )
}

pub fn expired(ref_id: &str) -> String {
    format!("A cobrança {ref_id} expirou sem pagamento.")
}

pub fn partial(received: &str, expected: &str, missing: &str, symbol: &str, ref_id: &str) -> String {
    format!(
        "Recebi apenas {received} {symbol} da cobrança de {expected} {symbol} ({ref_id}).\n\
         Faltam {missing} {symbol}."
    )
}

pub fn status_paid(ref_id: &str, when: &str) -> String {
    format!("Cobrança {ref_id}: paga em {when}.")
}

pub fn sweep_ready(pct: u8, amount: &str, symbol: &str, destination_short: &str) -> String {
    format!(
        "Separei {pct}% ({amount} {symbol}) para sua reserva de rendimento, como combinado.\n\
         A transação está pronta e vai para a carteira {destination_short}. \
         Falta só a assinatura do titular."
    )
}

/// `Abcd…WXYZ` shortening for wallet addresses in chat.
pub fn short_addr(addr: &str) -> String {
    if addr.len() <= 12 {
        return addr.to_string();
    }
    format!("{}…{}", &addr[..4], &addr[addr.len() - 4..])
}
