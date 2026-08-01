//! Amount parsing and Brazilian-style formatting.
//!
//! All arithmetic inside ClawPay is integer math on base units (u64, with
//! u128 intermediates). Decimal strings only exist at the boundary: parsing
//! user/config input and rendering Portuguese messages.

use crate::core::error::ClawErr;

/// Parse a decimal amount string into base units for a mint with `decimals`.
///
/// Accepts Brazilian and international formats: `150`, `150,25`, `150.25`,
/// `1.234,56`, `1,234.56`. When both separators appear, the last one is the
/// decimal separator and the other is treated as thousands grouping. A single
/// separator is always treated as the decimal separator (so `1.234` is one
/// point two three four, not one thousand — amounts here are small).
pub fn parse_amount(input: &str, decimals: u8) -> Result<u64, ClawErr> {
    let s: String = input.trim().chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return Err(ClawErr::invalid_args("empty amount"));
    }
    if s.starts_with('-') {
        return Err(ClawErr::invalid_args("negative amount"));
    }

    let last_dot = s.rfind('.');
    let last_comma = s.rfind(',');
    let normalized: String = match (last_dot, last_comma) {
        (Some(d), Some(c)) => {
            let (dec_sep, group_sep) = if d > c { ('.', ',') } else { (',', '.') };
            let mut out = String::with_capacity(s.len());
            let dec_pos = s.rfind(dec_sep).unwrap();
            for (i, ch) in s.char_indices() {
                if ch == group_sep {
                    continue;
                }
                if ch == dec_sep {
                    if i == dec_pos {
                        out.push('.');
                    }
                    // earlier occurrences of the decimal char are grouping
                    continue;
                }
                out.push(ch);
            }
            out
        }
        _ => s.replace(',', "."),
    };

    let mut parts = normalized.splitn(2, '.');
    let int_part = parts.next().unwrap_or("");
    let frac_part = parts.next().unwrap_or("");
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(ClawErr::invalid_args("amount has no digits"));
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(ClawErr::invalid_args(format!("non-numeric amount `{input}`")));
    }
    if frac_part.len() > decimals as usize {
        return Err(ClawErr::invalid_args(format!(
            "amount `{input}` has more than {decimals} decimal places"
        )));
    }

    let scale = 10u128.pow(decimals as u32);
    let int_val: u128 = if int_part.is_empty() {
        0
    } else {
        int_part
            .parse()
            .map_err(|_| ClawErr::invalid_args("amount integer part too large"))?
    };
    let mut frac_val: u128 = if frac_part.is_empty() {
        0
    } else {
        frac_part.parse().unwrap_or(0)
    };
    frac_val *= 10u128.pow((decimals as usize - frac_part.len()) as u32);

    let total = int_val
        .checked_mul(scale)
        .and_then(|v| v.checked_add(frac_val))
        .ok_or_else(|| ClawErr::invalid_args("amount overflows"))?;
    u64::try_from(total).map_err(|_| ClawErr::invalid_args("amount too large"))
}

/// Format base units Brazilian-style: dot thousands grouping, comma decimal
/// separator, at least two decimal places, trailing zeros beyond two trimmed.
/// `1_234_560_000` with 6 decimals renders as `1.234,56`.
pub fn format_amount(base: u64, decimals: u8) -> String {
    let scale = 10u64.pow(decimals as u32);
    let int_part = base / scale;
    let frac_part = base % scale;

    let int_str = int_part.to_string();
    let mut grouped = String::new();
    for (i, ch) in int_str.chars().enumerate() {
        if i > 0 && (int_str.len() - i).is_multiple_of(3) {
            grouped.push('.');
        }
        grouped.push(ch);
    }

    if decimals == 0 {
        return format!("{grouped},00");
    }
    let mut frac_str = format!("{frac_part:0width$}", width = decimals as usize);
    while frac_str.len() > 2 && frac_str.ends_with('0') {
        frac_str.pop();
    }
    format!("{grouped},{frac_str}")
}

/// Canonical plain decimal for Solana Pay URLs: `.` separator, no grouping,
/// no trailing fractional zeros (`150`, `150.5`).
pub fn url_amount(base: u64, decimals: u8) -> String {
    let scale = 10u64.pow(decimals as u32);
    let int_part = base / scale;
    let frac_part = base % scale;
    if frac_part == 0 || decimals == 0 {
        return int_part.to_string();
    }
    let mut frac_str = format!("{frac_part:0width$}", width = decimals as usize);
    while frac_str.ends_with('0') {
        frac_str.pop();
    }
    format!("{int_part}.{frac_str}")
}

/// `amount * pct / 100`, flooring, safe against overflow.
pub fn pct_of(amount: u64, pct: u8) -> u64 {
    ((amount as u128 * pct as u128) / 100) as u64
}

/// Validate a base58-encoded 32-byte Solana pubkey; returns the raw bytes.
pub fn validate_pubkey(s: &str) -> Result<[u8; 32], String> {
    let bytes = bs58::decode(s)
        .into_vec()
        .map_err(|e| format!("invalid base58: {e}"))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "pubkey is not 32 bytes".to_string())
}
