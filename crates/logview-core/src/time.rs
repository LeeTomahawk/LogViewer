//! Timestamp parsing.
//!
//! Timestamps are milliseconds since 1970-01-01 of the wall-clock time written
//! in the log ("naive" time). If the log states a UTC offset, the value is
//! converted to UTC; otherwise it is taken as is. Comparisons within one file
//! are what matters, so no local time zone is assumed.
//!
//! Common formats have hand-written parsers (fast enough for every line of a
//! large file); custom formats use chrono's `strftime` syntax.

use chrono::{DateTime, NaiveDate, NaiveDateTime};

/// Milliseconds since 1970-01-01T00:00:00 (naive or UTC).
pub type Millis = i64;

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn digits(s: &[u8], at: usize, n: usize) -> Option<u32> {
    let part = s.get(at..at + n)?;
    part.iter().try_fold(0u32, |acc, &b| {
        b.is_ascii_digit().then(|| acc * 10 + u32::from(b - b'0'))
    })
}

fn to_millis(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32, ms: u32) -> Option<Millis> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(((days * 86_400 + (h * 3600 + mi * 60 + s) as i64) * 1000) + ms as i64)
}

/// Parses `YYYY-MM-DD[T ]HH:MM[:SS][.,fraction][Z|±HH[:]MM]` at the start of
/// `s` (`/` is accepted as a date separator). Returns the time and the number
/// of bytes consumed.
pub fn parse_iso(s: &[u8]) -> Option<(Millis, usize)> {
    let y = digits(s, 0, 4)?;
    let sep = *s.get(4)?;
    if sep != b'-' && sep != b'/' || *s.get(7)? != sep {
        return None;
    }
    let mo = digits(s, 5, 2)?;
    let d = digits(s, 8, 2)?;
    if !matches!(s.get(10)?, b'T' | b' ' | b'_') || *s.get(13)? != b':' {
        return None;
    }
    let h = digits(s, 11, 2)?;
    let mi = digits(s, 14, 2)?;
    let mut pos = 16;
    let mut sec = 0;
    if s.get(pos) == Some(&b':') {
        sec = digits(s, pos + 1, 2)?;
        pos += 3;
    }
    let mut ms = 0;
    if matches!(s.get(pos), Some(b'.' | b',')) && s.get(pos + 1).is_some_and(u8::is_ascii_digit) {
        pos += 1;
        let start = pos;
        while s.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        // First three fraction digits are milliseconds.
        let frac = &s[start..pos];
        for (i, &b) in frac.iter().take(3).enumerate() {
            ms += u32::from(b - b'0') * 10u32.pow(2 - i as u32);
        }
    }
    let mut value = to_millis(y as i64, mo, d, h, mi, sec, ms)?;
    match s.get(pos) {
        Some(b'Z') => pos += 1,
        Some(&sign @ (b'+' | b'-')) => {
            if let Some(oh) = digits(s, pos + 1, 2) {
                let (om, len) = if s.get(pos + 3) == Some(&b':') {
                    (digits(s, pos + 4, 2), 6)
                } else {
                    (digits(s, pos + 3, 2), 5)
                };
                if let Some(om) = om {
                    let offset = (oh * 60 + om) as i64 * 60_000;
                    value += if sign == b'+' { -offset } else { offset };
                    pos += len;
                }
            }
        }
        _ => {}
    }
    Some((value, pos))
}

const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

fn month(s: &[u8]) -> Option<u32> {
    let m = s.get(..3)?;
    MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(m))
        .map(|i| i as u32 + 1)
}

/// Parses syslog `Mon dd HH:MM:SS` (no year; `year` is used).
pub fn parse_syslog(s: &[u8], year: i64) -> Option<(Millis, usize)> {
    let mo = month(s)?;
    let mut pos = 3;
    while s.get(pos) == Some(&b' ') {
        pos += 1;
    }
    if pos == 3 {
        return None;
    }
    let d_len = if s.get(pos + 1).is_some_and(u8::is_ascii_digit) {
        2
    } else {
        1
    };
    let d = digits(s, pos, d_len)?;
    pos += d_len;
    if s.get(pos) != Some(&b' ') || s.get(pos + 3) != Some(&b':') || s.get(pos + 6) != Some(&b':') {
        return None;
    }
    let h = digits(s, pos + 1, 2)?;
    let mi = digits(s, pos + 4, 2)?;
    let sec = digits(s, pos + 7, 2)?;
    Some((to_millis(year, mo, d, h, mi, sec, 0)?, pos + 9))
}

/// Parses Apache/nginx `dd/Mon/YYYY:HH:MM:SS ±HHMM`.
pub fn parse_apache(s: &[u8]) -> Option<(Millis, usize)> {
    let d = digits(s, 0, 2)?;
    if s.get(2) != Some(&b'/') || s.get(6) != Some(&b'/') || s.get(11) != Some(&b':') {
        return None;
    }
    let mo = month(s.get(3..6)?)?;
    let y = digits(s, 7, 4)?;
    let h = digits(s, 12, 2)?;
    let mi = digits(s, 15, 2)?;
    let sec = digits(s, 18, 2)?;
    let mut value = to_millis(y as i64, mo, d, h, mi, sec, 0)?;
    let mut pos = 20;
    if s.get(pos) == Some(&b' ')
        && let Some(&sign @ (b'+' | b'-')) = s.get(pos + 1)
        && let (Some(oh), Some(om)) = (digits(s, pos + 2, 2), digits(s, pos + 4, 2))
    {
        let offset = (oh * 60 + om) as i64 * 60_000;
        value += if sign == b'+' { -offset } else { offset };
        pos += 6;
    }
    Some((value, pos))
}

/// Unix time as 10 digits (seconds) or 13 digits (milliseconds), optionally
/// with a fraction (`1767225600.123`).
pub fn parse_epoch(s: &[u8]) -> Option<Millis> {
    let s = s.trim_ascii();
    let (int, frac) = match s.iter().position(|&b| b == b'.') {
        Some(p) => (&s[..p], &s[p + 1..]),
        None => (s, &b""[..]),
    };
    if !int.iter().all(u8::is_ascii_digit) || !frac.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value: i64 = std::str::from_utf8(int).ok()?.parse().ok()?;
    match int.len() {
        10 => {
            let ms = frac
                .iter()
                .take(3)
                .enumerate()
                .map(|(i, &b)| i64::from(b - b'0') * 10i64.pow(2 - i as u32))
                .sum::<i64>();
            Some(value * 1000 + ms)
        }
        13 => Some(value),
        _ => None,
    }
}

/// Current year (UTC), used for syslog timestamps without a year.
pub fn current_year() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    civil_from_days(secs.div_euclid(86_400)).0
}

/// Tries every known format on `s` (a whole field, or the start of a line;
/// a leading `[` is skipped).
pub fn parse_auto(s: &[u8], year: i64) -> Option<Millis> {
    let s = s.trim_ascii_start();
    let s = s.strip_prefix(b"[").unwrap_or(s);
    parse_iso(s)
        .or_else(|| parse_apache(s))
        .or_else(|| parse_syslog(s, year))
        .map(|(t, _)| t)
        .or_else(|| parse_epoch(s))
}

/// Parses `s` with a chrono `strftime` format. Formats with `%z`/`%:z` are
/// converted to UTC; a date-only format means midnight.
pub fn parse_with_format(s: &str, format: &str) -> Option<Millis> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_str(s, format) {
        return Some(t.timestamp_millis());
    }
    if let Ok(t) = NaiveDateTime::parse_from_str(s, format) {
        return Some(t.and_utc().timestamp_millis());
    }
    NaiveDate::parse_from_str(s, format)
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc().timestamp_millis())
}

/// Whether a chrono format string is valid (parses a sample it formats).
pub fn check_format(format: &str) -> Result<(), String> {
    use chrono::format::{Item, StrftimeItems};
    if StrftimeItems::new(format).any(|i| matches!(i, Item::Error)) {
        Err(format!("nieprawidłowy format czasu: {format}"))
    } else {
        Ok(())
    }
}

/// `YYYY-MM-DD HH:MM:SS` for a time in seconds.
pub fn format_secs(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Parses user input `YYYY-MM-DD[ HH:MM[:SS]]` into seconds.
pub fn parse_user_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() == 10 {
        let b = s.as_bytes();
        let (y, m, d) = (digits(b, 0, 4)?, digits(b, 5, 2)?, digits(b, 8, 2)?);
        if b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        return to_millis(y as i64, m, d, 0, 0, 0, 0).map(|ms| ms / 1000);
    }
    match parse_iso(s.as_bytes()) {
        Some((ms, len)) if len == s.len() => Some(ms.div_euclid(1000)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Millis = 1_767_225_600_000; // 2026-01-01T00:00:00

    #[test]
    fn civil_round_trip() {
        for days in [-719_468, -1, 0, 1, 20_454, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(2026, 1, 1) * 86_400_000, T0);
    }

    #[test]
    fn iso_variants() {
        assert_eq!(parse_iso(b"2026-01-01 00:00:00"), Some((T0, 19)));
        assert_eq!(
            parse_iso(b"2026-01-01T00:00:00.123 INFO"),
            Some((T0 + 123, 23))
        );
        assert_eq!(parse_iso(b"2026-01-01 00:00:00,5"), Some((T0 + 500, 21)));
        assert_eq!(
            parse_iso(b"2026-01-01T00:00:00.123456Z"),
            Some((T0 + 123, 27))
        );
        assert_eq!(parse_iso(b"2026/01/01 00:00"), Some((T0, 16)));
        assert_eq!(parse_iso(b"2026-01-01T02:00:00+02:00"), Some((T0, 25)));
        assert_eq!(
            parse_iso(b"2026-01-01T00:00:00-0130"),
            Some((T0 + 90 * 60_000, 24))
        );
        assert_eq!(parse_iso(b"2026-13-01 00:00:00"), None);
        assert_eq!(parse_iso(b"2026-01-01X00:00:00"), None);
        assert_eq!(parse_iso(b"2026-01"), None);
    }

    #[test]
    fn syslog() {
        assert_eq!(parse_syslog(b"Jan  1 00:00:00 host", 2026), Some((T0, 15)));
        assert_eq!(parse_syslog(b"Jan 01 00:00:00", 2026), Some((T0, 15)));
        assert_eq!(parse_syslog(b"Foo  1 00:00:00", 2026), None);
    }

    #[test]
    fn apache() {
        assert_eq!(parse_apache(b"01/Jan/2026:01:00:00 +0100"), Some((T0, 26)));
        assert_eq!(parse_apache(b"01/Jan/2026:00:00:00"), Some((T0, 20)));
    }

    #[test]
    fn epoch() {
        assert_eq!(parse_epoch(b"1767225600"), Some(T0));
        assert_eq!(parse_epoch(b"1767225600.25"), Some(T0 + 250));
        assert_eq!(parse_epoch(b"1767225600000"), Some(T0));
        assert_eq!(parse_epoch(b"12345"), None);
        assert_eq!(parse_epoch(b"17672256x0"), None);
    }

    #[test]
    fn auto_skips_bracket() {
        assert_eq!(parse_auto(b"[2026-01-01 00:00:00] x", 2026), Some(T0));
        assert_eq!(parse_auto(b"01/Jan/2026:00:00:00 +0000", 2026), Some(T0));
        assert_eq!(parse_auto(b"hello", 2026), None);
    }

    #[test]
    fn custom_format() {
        assert_eq!(
            parse_with_format("01.01.2026 00:00:00", "%d.%m.%Y %H:%M:%S"),
            Some(T0)
        );
        assert_eq!(parse_with_format("01.01.2026", "%d.%m.%Y"), Some(T0));
        assert_eq!(
            parse_with_format("2026-01-01 01:00:00 +0100", "%Y-%m-%d %H:%M:%S %z"),
            Some(T0)
        );
        assert_eq!(parse_with_format("nope", "%d.%m.%Y"), None);
        assert!(check_format("%d.%m.%Y").is_ok());
        assert!(check_format("%Q").is_err());
    }

    #[test]
    fn user_time() {
        assert_eq!(parse_user_time("2026-01-01"), Some(T0 / 1000));
        assert_eq!(parse_user_time("2026-01-01 00:01"), Some(T0 / 1000 + 60));
        assert_eq!(
            parse_user_time(" 2026-01-01 00:00:05 "),
            Some(T0 / 1000 + 5)
        );
        assert_eq!(parse_user_time("2026-01-01 00:00:05 junk"), None);
        assert_eq!(format_secs(T0 / 1000 + 3661), "2026-01-01 01:01:01");
    }
}
