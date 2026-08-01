//! Minimal civil-time helpers (no chrono dependency).
//!
//! ClawPay only needs two things from a calendar: "today's local midnight"
//! for daily caps, and `dd/mm/yyyy às HH:MM` for Portuguese messages. Both
//! are computed from unix seconds plus a fixed UTC offset (default Brasília,
//! UTC-3; Brazil has no DST since 2019).

/// Days-from-civil / civil-from-days by Howard Hinnant's algorithms.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix timestamp of the most recent local midnight for the given offset.
pub fn local_midnight(now_unix: i64, utc_offset_hours: i32) -> i64 {
    let offset = utc_offset_hours as i64 * 3600;
    let local = now_unix + offset;
    let local_midnight = local.div_euclid(86_400) * 86_400;
    local_midnight - offset
}

/// `01/08/2026 às 14:32` in local time.
pub fn format_datetime_pt(unix: i64, utc_offset_hours: i32) -> String {
    let local = unix + utc_offset_hours as i64 * 3600;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm) = (secs / 3600, (secs % 3600) / 60);
    format!("{d:02}/{m:02}/{y:04} às {hh:02}:{mm:02}")
}

/// `23:59 de 01/08/2026` in local time (used for invoice validity).
pub fn format_deadline_pt(unix: i64, utc_offset_hours: i32) -> String {
    let local = unix + utc_offset_hours as i64 * 3600;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm) = (secs / 3600, (secs % 3600) / 60);
    format!("{hh:02}:{mm:02} de {d:02}/{m:02}/{y:04}")
}
