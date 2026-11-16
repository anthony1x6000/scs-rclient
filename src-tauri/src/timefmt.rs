//! In-house date/time handling: WebDAV HTTP-date parsing and the local
//! `YYYY-MM-DD` used to name dated backups.
//!
//! Replaces the `chrono` dependency, which was used at five call sites for
//! fixed-shape text <-> epoch conversions and pulled a timezone-database
//! reader into the crate graph. The two OS calls below are the only unsafe
//! code: `localtime_r` / `GetLocalTime` for the local calendar date.

use std::time::{SystemTime, UNIX_EPOCH};

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn month_number(token: &str) -> Option<u32> {
    let name = token.trim_end_matches('.').to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|month| *month == name)
        .map(|index| index as u32 + 1)
}

/// Howard Hinnant's days-from-civil: days since 1970-01-01, `None` for an
/// invalid calendar date (including leap-day checks).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day > month_lengths[(month - 1) as usize] {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month as i64 + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// Inverse of [`days_from_civil`], used only for the UTC fallback.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

fn parse_time(token: &str) -> Option<(i64, i64, i64)> {
    let mut parts = token.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

fn parse_zone(token: &str) -> Option<i64> {
    match token.to_ascii_uppercase().as_str() {
        "GMT" | "UTC" | "UT" | "Z" => return Some(0),
        _ => {}
    }
    let sign = match token.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &token[1..];
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if rest.len() == 4 => (&rest[..2], &rest[2..]),
        _ => return None,
    };
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

fn to_epoch(
    year: i64,
    month: u32,
    day: u32,
    hour: i64,
    minute: i64,
    second: i64,
    offset_seconds: i64,
) -> Option<u64> {
    let days = days_from_civil(year, month, day)?;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(seconds.max(0) as u64)
}

/// RFC 3339: `2026-09-28T13:45:09Z` / `...+05:30` / fractional seconds.
fn parse_rfc3339(input: &str) -> Option<u64> {
    let separator = input.find(['T', 't'])?;
    let (date, rest) = input.split_at(separator);
    let rest = &rest[1..];

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let (time, offset) = if let Some(time) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z'))
    {
        (time, 0)
    } else {
        let zone_at = rest.rfind(|c| c == '+' || c == '-')?;
        (&rest[..zone_at], parse_zone(&rest[zone_at..])?)
    };
    let time = time.split('.').next()?;
    let (hour, minute, second) = parse_time(time)?;
    to_epoch(year, month, day, hour, minute, second, offset)
}

/// RFC 2822 / RFC 1123: `Mon, 28 Sep 2026 13:45:09 GMT`.
fn parse_rfc2822(input: &str) -> Option<u64> {
    let mut tokens: Vec<&str> = input.split_whitespace().collect();
    if tokens.first()?.ends_with(',') {
        tokens.remove(0);
    }
    if tokens.len() < 5 {
        return None;
    }
    let day: u32 = tokens[0].parse().ok()?;
    let month = month_number(tokens[1])?;
    let year_token = tokens[2];
    let mut year: i64 = year_token.parse().ok()?;
    if year_token.len() == 2 {
        year = if year >= 69 { 1900 + year } else { 2000 + year };
    }
    let (hour, minute, second) = parse_time(tokens[3])?;
    let offset = parse_zone(tokens[4])?;
    to_epoch(year, month, day, hour, minute, second, offset)
}

/// RFC 850: `Monday, 28-Sep-26 13:45:09 GMT` (two-digit year rolls at 69).
fn parse_rfc850(input: &str) -> Option<u64> {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    if tokens.len() < 4 {
        return None;
    }
    let (date_token, time_index) = if tokens[0].ends_with(',') {
        (tokens.get(1).copied()?, 2)
    } else {
        (tokens[0], 1)
    };
    let mut parts = date_token.split('-');
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let two_digit_year: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let year = if two_digit_year >= 69 {
        1900 + two_digit_year
    } else {
        2000 + two_digit_year
    };
    let (hour, minute, second) = parse_time(tokens.get(time_index)?)?;
    let offset = parse_zone(tokens.get(time_index + 1)?)?;
    to_epoch(year, month, day, hour, minute, second, offset)
}

/// asctime: `Mon Sep 28 13:45:09 2026`.
fn parse_asctime(input: &str) -> Option<u64> {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    if tokens.len() != 5 {
        return None;
    }
    let month = month_number(tokens[1])?;
    let day: u32 = tokens[2].parse().ok()?;
    let (hour, minute, second) = parse_time(tokens[3])?;
    let year: i64 = tokens[4].parse().ok()?;
    to_epoch(year, month, day, hour, minute, second, 0)
}

/// Parses the HTTP-date shapes WebDAV servers put in `getlastmodified`.
/// Returns seconds since the Unix epoch, clamped at zero like the previous
/// implementation.
pub fn parse_webdav_date(date_str: &str) -> Option<u64> {
    let trimmed = date_str.trim();
    if trimmed.is_empty() {
        return None;
    }
    parse_rfc3339(trimmed)
        .or_else(|| parse_rfc2822(trimmed))
        .or_else(|| parse_rfc850(trimmed))
        .or_else(|| parse_asctime(trimmed))
}

/// Local calendar date as `YYYY-MM-DD`, used for `backups/<date>` folders.
pub fn local_date_ymd() -> String {
    let (year, month, day) = local_ymd();
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(unix)]
fn local_ymd() -> (i32, u32, u32) {
    // SAFETY: `time` and `localtime_r` write into stack values we own; the
    // tm pointer is valid for the call and we copy the fields immediately.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if !libc::localtime_r(&now, &mut tm).is_null() {
            return (tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32);
        }
    }
    utc_ymd_now()
}

#[cfg(windows)]
fn local_ymd() -> (i32, u32, u32) {
    // SAFETY: GetLocalTime only writes into the SYSTEMTIME we own.
    unsafe {
        let mut local: windows_sys::Win32::Foundation::SYSTEMTIME = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut local);
        (local.wYear as i32, local.wMonth as u32, local.wDay as u32)
    }
}

fn utc_ymd_now() -> (i32, u32, u32) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    (year as i32, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_EPOCH: u64 = 1_790_603_109; // 2026-09-28T13:45:09Z

    #[test]
    fn parses_rfc1123_to_exact_epoch() {
        assert_eq!(
            parse_webdav_date("Mon, 28 Sep 2026 13:45:09 GMT"),
            Some(SAMPLE_EPOCH)
        );
        assert_eq!(
            parse_webdav_date("Mon, 28 Sep 2026 13:45:09 +0000"),
            Some(SAMPLE_EPOCH)
        );
    }

    #[test]
    fn parses_rfc3339_offsets_and_fractions() {
        assert_eq!(parse_webdav_date("2026-09-28T13:45:09Z"), Some(SAMPLE_EPOCH));
        assert_eq!(
            parse_webdav_date("2026-09-28T19:15:09+05:30"),
            Some(SAMPLE_EPOCH)
        );
        assert_eq!(
            parse_webdav_date("2026-09-28T08:45:09-05:00"),
            Some(SAMPLE_EPOCH)
        );
        assert_eq!(
            parse_webdav_date("2026-09-28T13:45:09.123Z"),
            Some(SAMPLE_EPOCH)
        );
    }

    #[test]
    fn parses_legacy_rfc850_and_asctime() {
        assert_eq!(
            parse_webdav_date("Monday, 28-Sep-26 13:45:09 GMT"),
            Some(SAMPLE_EPOCH)
        );
        assert_eq!(
            parse_webdav_date("Mon Sep 28 13:45:09 2026"),
            Some(SAMPLE_EPOCH)
        );
    }

    #[test]
    fn two_digit_years_roll_at_69() {
        assert_eq!(
            parse_webdav_date("Sunday, 28-Sep-69 13:45:09 GMT"),
            Some(0)
        );
        assert_eq!(
            parse_webdav_date("Friday, 28-Sep-68 13:45:09 GMT"),
            Some(3_116_065_509)
        );
    }

    #[test]
    fn validates_calendar_and_clock_ranges() {
        assert_eq!(
            parse_webdav_date("2024-02-29T00:00:00Z"),
            Some(1_709_164_800)
        );
        assert!(parse_webdav_date("2100-02-29T00:00:00Z").is_none());
        assert!(parse_webdav_date("2026-02-30T00:00:00Z").is_none());
        assert!(parse_webdav_date("2026-09-28T25:00:00Z").is_none());
    }

    #[test]
    fn rejects_invalid_dates() {
        assert!(parse_webdav_date("").is_none());
        assert!(parse_webdav_date("invalid date string").is_none());
        assert!(parse_webdav_date("Mon, 32 Foo 2026 99:99:99 GMT").is_none());
        assert!(parse_webdav_date("2026-09-28").is_none());
    }

    #[test]
    fn clamps_pre_epoch_dates() {
        assert_eq!(parse_webdav_date("1969-12-31T23:59:59Z"), Some(0));
    }

    #[test]
    fn local_date_has_iso_shape() {
        let date = local_date_ymd();
        assert_eq!(date.len(), 10, "unexpected date shape: {date}");
        let bytes = date.as_bytes();
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        for (index, byte) in bytes.iter().enumerate() {
            if index != 4 && index != 7 {
                assert!(byte.is_ascii_digit(), "unexpected date shape: {date}");
            }
        }
    }
}
