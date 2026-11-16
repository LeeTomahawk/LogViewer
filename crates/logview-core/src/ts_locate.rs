//! Builds a log format from a timestamp the user marked in a sample line.
//!
//! The user selects the timestamp in one line; [`locate`] then works out
//!
//! * **where** it is: at the start of the line, after the N-th
//!   whitespace-separated field, or right after a fixed text such as `time=`,
//! * **what it looks like**: the selected text generalized to a regex
//!   (digits → `\d+`, letters → `[A-Za-z]+`, spaces → `\s+`),
//! * **how to read it**: automatically (ISO, syslog, Apache, epoch) or with one
//!   of the common `strftime` formats from [`TIME_FORMATS`].
//!
//! The result is a regex format with groups `pre` (text before the time,
//! if any), `ts` and `msg`. Lines that do not match it are continuation lines
//! of the previous record, as with any regex format.

use std::ops::Range;

use crate::format::{FormatKind, LogFormat};
use crate::time::{self, Millis};

/// `strftime` formats tried, in order, when the automatic parsers do not
/// recognize the selection. Day-first dates come before month-first ones.
pub const TIME_FORMATS: &[&str] = &[
    "%d.%m.%Y %H:%M:%S%.f",
    "%d.%m.%Y %H:%M:%S,%3f",
    "%d.%m.%Y %H:%M",
    "%d.%m.%Y, %H:%M:%S",
    "%d-%m-%Y %H:%M:%S%.f",
    "%d/%m/%Y %H:%M:%S%.f",
    "%d/%m/%Y %H:%M",
    "%m/%d/%Y %H:%M:%S%.f",
    "%m/%d/%Y %I:%M:%S %p",
    "%Y.%m.%d %H:%M:%S%.f",
    "%Y/%m/%d %H:%M:%S%.f",
    "%Y%m%d %H:%M:%S%.f",
    "%Y%m%dT%H%M%S%.f",
    "%Y%m%d%H%M%S",
    "%Y-%m-%d %H:%M:%S %z",
    "%Y-%m-%d %H:%M:%S,%3f",
    "%d-%b-%Y %H:%M:%S%.f",
    "%d %b %Y %H:%M:%S%.f",
    "%b %d, %Y %I:%M:%S %p",
    "%b %d %Y %H:%M:%S",
    "%a %b %e %H:%M:%S %Y",
    "%a, %d %b %Y %H:%M:%S %z",
    "%d.%m.%Y",
    "%d/%m/%Y",
    "%Y-%m-%d",
    "%H:%M:%S%.f",
    "%H:%M:%S,%3f",
];

/// Result of [`locate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimestampGuess {
    /// Regex format with the `ts` group; `timestamp_format` is empty for
    /// automatic parsing or when the format was not recognized.
    pub format: LogFormat,
    /// Whether the selected text could be read as a time.
    pub recognized: bool,
    /// The time read from the sample line.
    pub example: Option<Millis>,
}

/// How to read `text` as a time: `Some("")` = the automatic parsers,
/// `Some(fmt)` = a `strftime` format from [`TIME_FORMATS`].
pub fn detect_time_format(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if time::parse_auto(text.as_bytes(), time::current_year()).is_some() {
        return Some(String::new());
    }
    TIME_FORMATS
        .iter()
        .find(|f| time::parse_with_format(text, f).is_some())
        .map(|f| (*f).to_owned())
}

/// Builds a format from the timestamp at byte range `selection` of `line`.
pub fn locate(line: &str, selection: Range<usize>) -> Result<TimestampGuess, String> {
    let Range { start, end } = selection;
    if start >= end
        || end > line.len()
        || !line.is_char_boundary(start)
        || !line.is_char_boundary(end)
    {
        return Err("Zaznacz myszą czas w linii.".into());
    }
    // Ignore spaces selected around the time.
    let selected = &line[start..end];
    let start = start + (selected.len() - selected.trim_start().len());
    let end = end - (selected.len() - selected.trim_end().len());
    if start >= end {
        return Err("Zaznacz myszą czas w linii.".into());
    }
    let text = &line[start..end];

    let detected = detect_time_format(text);
    let pattern = format!(
        "{}(?P<ts>{}){}\\s*(?P<msg>.*)$",
        prefix_regex(&line[..start]),
        generalize(text),
        suffix_regex(&line[end..]),
    );
    let format = LogFormat {
        name: "Własny".into(),
        kind: FormatKind::Regex(pattern),
        timestamp_format: detected.clone().unwrap_or_default(),
    };
    let compiled = format.compile(&[line.as_bytes()])?;
    let Some(meta) = compiled.meta(line.as_bytes()) else {
        return Err("Wyznaczone wyrażenie nie pasuje do tej linii.".into());
    };
    let example = if detected.is_some() { meta.ts } else { None };
    if detected.is_some() && example.is_none() {
        return Err(format!("Nie udało się odczytać czasu „{text}”."));
    }
    Ok(TimestampGuess {
        format,
        recognized: detected.is_some(),
        example,
    })
}

/// Regex for the text before the time.
fn prefix_regex(pre: &str) -> String {
    if pre.is_empty() {
        return "^".into();
    }
    if pre.trim().is_empty() {
        return r"^\s*".into();
    }
    if pre == "[" {
        // Same shape as the built-in formats (keeps the fast parsing path).
        return r"^\[?".into();
    }
    // `tail` = the part of the field the time starts in (`time=`, `[`, …).
    let (head, tail) = match pre.rfind(char::is_whitespace) {
        Some(i) => {
            let ws = pre[i..].chars().next().map_or(1, char::len_utf8);
            (&pre[..i + ws], &pre[i + ws..])
        }
        None => ("", pre),
    };
    if tail.chars().any(char::is_alphanumeric) {
        // A key such as `time=` or `ts:`: find it anywhere.
        return format!("^(?P<pre>.*?{})", regex::escape(tail));
    }
    let fields = head.split_whitespace().count();
    let skip = match fields {
        0 => String::new(),
        n => format!(r"(?:\S+\s+){{{n}}}"),
    };
    format!(r"^(?P<pre>\s*{skip}{})", regex::escape(tail))
}

/// Optional closing character right after the time (`]`, `"`, …).
fn suffix_regex(post: &str) -> String {
    match post.chars().next() {
        Some(c @ (']' | ')' | '}' | '>' | '"' | '\'' | '|' | ',' | ';')) => {
            format!("{}?", regex::escape(&c.to_string()))
        }
        _ => String::new(),
    }
}

/// The selected time as a regex: digit, letter and space runs become
/// classes, other characters are kept literally.
fn generalize(text: &str) -> String {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Digit,
        Alpha,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_ascii_digit() {
            Class::Digit
        } else if c.is_ascii_alphabetic() {
            Class::Alpha
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut out = String::new();
    let mut last = None;
    for c in text.chars() {
        let k = class(c);
        match k {
            Class::Other => out.push_str(&regex::escape(&c.to_string())),
            _ if last == Some(k) => continue,
            Class::Digit => out.push_str(r"\d+"),
            Class::Alpha => out.push_str("[A-Za-z]+"),
            Class::Space => out.push_str(r"\s+"),
        }
        last = Some(k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Millis = 1_767_225_600_000; // 2026-01-01 00:00:00 UTC

    fn guess(line: &str, ts: &str) -> TimestampGuess {
        let start = line.find(ts).expect("ts in line");
        locate(line, start..start + ts.len()).unwrap()
    }

    fn regex(g: &TimestampGuess) -> &str {
        match &g.format.kind {
            FormatKind::Regex(r) => r,
            _ => unreachable!(),
        }
    }

    fn ts_of(g: &TimestampGuess, line: &str) -> Option<Millis> {
        let f = g.format.compile(&[]).unwrap();
        f.meta(line.as_bytes()).and_then(|m| m.ts)
    }

    #[test]
    fn time_at_line_start() {
        let line = "2026-01-01 00:00:05.250 INFO start";
        let g = guess(line, "2026-01-01 00:00:05.250");
        assert!(g.recognized);
        assert_eq!(g.format.timestamp_format, "");
        assert_eq!(g.example, Some(T0 + 5250));
        assert!(regex(&g).starts_with("^(?P<ts>"), "{}", regex(&g));
        assert_eq!(ts_of(&g, "2026-01-01 00:00:06.000 WARN x"), Some(T0 + 6000));
        assert_eq!(ts_of(&g, "\tat Foo.bar()"), None, "continuation");
    }

    #[test]
    fn time_after_fields() {
        let line = "web-01 12345 01.01.2026 00:00:10 ERROR boom";
        let g = guess(line, "01.01.2026 00:00:10");
        assert_eq!(g.format.timestamp_format, "%d.%m.%Y %H:%M:%S%.f");
        assert_eq!(g.example, Some(T0 + 10_000));
        assert_eq!(
            ts_of(&g, "db-2 7 02.01.2026 00:00:00 INFO ok"),
            Some(T0 + 86_400_000)
        );
        let f = g.format.compile(&[]).unwrap();
        assert_eq!(f.columns(), ["pre", "ts", "msg"]);
        let level = f.meta(line.as_bytes()).unwrap().level;
        assert_eq!(level, Some(crate::Level::Error));
    }

    #[test]
    fn time_in_brackets_and_after_key() {
        let line = "[2026-01-01T00:00:01Z] started";
        let g = guess(line, "2026-01-01T00:00:01Z");
        assert!(regex(&g).starts_with(r"^\[?(?P<ts>"));
        assert_eq!(g.example, Some(T0 + 1000));

        let line = "level=info time=\"01/01/2026 00:01:00\" msg=hi";
        let g = guess(line, "01/01/2026 00:01:00");
        assert!(regex(&g).contains(r#"time=""#), "{}", regex(&g));
        assert_eq!(g.example, Some(T0 + 60_000));
        assert_eq!(
            ts_of(&g, "level=warn x=1 time=\"01/01/2026 00:02:00\" msg=y"),
            Some(T0 + 120_000)
        );
    }

    #[test]
    fn unknown_time_is_located_but_not_recognized() {
        let line = "tick 0042 something";
        let g = guess(line, "0042");
        assert!(!g.recognized);
        assert_eq!(g.example, None);
        assert_eq!(g.format.timestamp_format, "");
    }

    #[test]
    fn selection_is_trimmed_and_checked() {
        let line = "x 2026-01-01 00:00:00 y";
        assert!(locate(line, 3..3).is_err());
        assert!(locate(line, 0..100).is_err());
        let g = locate(line, 1..22).unwrap();
        assert_eq!(g.example, Some(T0));
    }

    #[test]
    fn known_formats() {
        for (text, ms) in [
            ("01.01.2026 00:00:00,250", T0 + 250),
            ("Jan 01 2026 00:00:00", T0),
            ("Thu Jan  1 00:00:00 2026", T0),
            ("01-Jan-2026 00:00:00.5", T0 + 500),
        ] {
            let f = detect_time_format(text).unwrap_or_else(|| panic!("{text}"));
            let value = if f.is_empty() {
                time::parse_auto(text.as_bytes(), 2026)
            } else {
                time::parse_with_format(text, &f)
            };
            assert_eq!(value, Some(ms), "{text} with {f:?}");
        }
        assert!(detect_time_format("12:30:00.100").is_some(), "time only");
        assert!(detect_time_format("hello").is_none());
    }
}
