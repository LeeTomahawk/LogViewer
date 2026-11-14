//! Log line formats: parsing of fields, timestamp and level.
//!
//! A [`LogFormat`] is a regex with named groups, JSON lines, or plain text.
//! Groups / keys named `ts` (timestamp) and `level` are special: they feed the
//! level and time filters. A line that does not match a regex format (or is
//! not a JSON object) is a *continuation* of the previous record (e.g. a stack
//! trace line).
//!
//! When there is no `level` field, the first level word in the first
//! [`LEVEL_SCAN_BYTES`] bytes of the line is used.

use std::borrow::Cow;

use regex::bytes::{Regex, RegexBuilder};

use crate::level::Level;
use crate::time::{self, Millis};

/// How far into a line to look for a level word when the format has no
/// `level` field.
pub const LEVEL_SCAN_BYTES: usize = 256;
/// Maximum number of JSON keys shown as columns.
const MAX_JSON_COLUMNS: usize = 16;

const ISO_TS: &str =
    r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2})?(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?";
const LEVELS: &str = r"(?i:TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|ERR|FATAL|CRITICAL|SEVERE)";

const TS_KEYS: &[&str] = &[
    "ts",
    "timestamp",
    "@timestamp",
    "time",
    "datetime",
    "date",
    "t",
];
const LEVEL_KEYS: &[&str] = &[
    "level",
    "severity",
    "lvl",
    "loglevel",
    "log.level",
    "levelname",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatKind {
    /// Regex with named groups, matched against the start of each line.
    Regex(String),
    /// One JSON object per line.
    Json,
    /// No structure: every line is a record; timestamp/level are guessed.
    Plain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFormat {
    pub name: String,
    pub kind: FormatKind,
    /// chrono `strftime` format for the `ts` field; empty = automatic.
    pub timestamp_format: String,
}

impl LogFormat {
    pub fn regex(name: &str, pattern: &str) -> Self {
        Self {
            name: name.to_owned(),
            kind: FormatKind::Regex(pattern.to_owned()),
            timestamp_format: String::new(),
        }
    }

    pub fn plain() -> Self {
        Self {
            name: "Zwykły tekst".into(),
            kind: FormatKind::Plain,
            timestamp_format: String::new(),
        }
    }

    pub fn json() -> Self {
        Self {
            name: "JSON lines".into(),
            kind: FormatKind::Json,
            timestamp_format: String::new(),
        }
    }

    /// Built-in formats, most specific first.
    pub fn builtins() -> Vec<Self> {
        vec![
            Self::regex(
                "Czas + poziom [wątek] logger - wiadomość",
                &format!(
                    r"^\[?(?P<ts>{ISO_TS})\]?\s+\[?(?P<level>{LEVELS})\]?\s+(?:\[(?P<thread>[^\]]*)\]\s+)?(?:(?P<logger>\S+)\s+-\s+)?(?P<msg>.*)$"
                ),
            ),
            Self::regex(
                "Poziom + czas + wiadomość",
                &format!(r"^\[?(?P<level>{LEVELS})\]?\s+\[?(?P<ts>{ISO_TS})\]?\s+(?P<msg>.*)$"),
            ),
            Self::regex(
                "Syslog",
                r"^(?P<ts>[A-Z][a-z]{2}\s+\d{1,2}\s\d{2}:\d{2}:\d{2})\s+(?P<host>\S+)\s+(?P<process>[^:\[\s]+)(?:\[(?P<pid>\d+)\])?:\s*(?P<msg>.*)$",
            ),
            Self::regex(
                "Apache/nginx (combined)",
                r#"^(?P<client>\S+) \S+ (?P<user>\S+) \[(?P<ts>[^\]]+)\] "(?P<request>[^"]*)" (?P<status>\d{3}) (?P<size>\S+)(?: "(?P<referer>[^"]*)" "(?P<agent>[^"]*)")?"#,
            ),
            Self::regex(
                "Czas + wiadomość",
                &format!(r"^\[?(?P<ts>{ISO_TS})\]?\s+(?P<msg>.*)$"),
            ),
            Self::json(),
            Self::plain(),
        ]
    }

    /// Compiles the format. JSON columns are taken from the keys of `sample`.
    pub fn compile(&self, sample: &[&[u8]]) -> Result<CompiledFormat, String> {
        if !self.timestamp_format.is_empty() {
            time::check_format(&self.timestamp_format)?;
        }
        let inner = match &self.kind {
            FormatKind::Regex(pattern) => {
                let regex = RegexBuilder::new(pattern)
                    .size_limit(16 << 20)
                    .build()
                    .map_err(|e| match e {
                        regex::Error::Syntax(s) => s,
                        other => other.to_string(),
                    })?;
                let columns: Vec<(String, usize)> = regex
                    .capture_names()
                    .enumerate()
                    .filter_map(|(i, n)| n.map(|n| (n.to_owned(), i)))
                    .collect();
                if columns.is_empty() {
                    return Err(
                        "wyrażenie musi mieć nazwane grupy, np. (?P<ts>…) (?P<level>…) (?P<msg>…)"
                            .into(),
                    );
                }
                let fast_meta = fast_meta_possible(pattern, &columns, &self.timestamp_format);
                Inner::Regex {
                    regex,
                    columns,
                    fast_meta,
                }
            }
            FormatKind::Json => Inner::Json {
                keys: json_keys(sample),
            },
            FormatKind::Plain => Inner::Plain,
        };
        let columns = match &inner {
            Inner::Regex { columns, .. } => columns.iter().map(|(n, _)| n.clone()).collect(),
            Inner::Json { keys } => keys.clone(),
            Inner::Plain => Vec::new(),
        };
        let ts_column = find_column(&columns, TS_KEYS);
        let level_column = find_column(&columns, LEVEL_KEYS);
        Ok(CompiledFormat {
            name: self.name.clone(),
            inner,
            columns,
            ts_column,
            level_column,
            timestamp_format: self.timestamp_format.clone(),
            year: time::current_year(),
            // ASCII word boundaries keep this on the fast DFA engine (a
            // Unicode `\b` falls back to a slower one on non-ASCII text).
            level_word: Regex::new(&format!(r"(?-u:\b){LEVELS}(?-u:\b)"))
                .expect("valid level regex"),
        })
    }
}

/// Whether [`CompiledFormat::meta`] may skip capture groups (≈8× faster):
/// the line must start with the `ts` group (optionally after `\[?`), the
/// timestamp format must be automatic, and a `level` group, if any, must come
/// right after `ts`. Then the header test is a plain `is_match`, the
/// timestamp is parsed from the start of the line and the level is the first
/// level word, which gives the same result as reading the groups.
fn fast_meta_possible(pattern: &str, columns: &[(String, usize)], ts_format: &str) -> bool {
    let rest = pattern.strip_prefix('^').unwrap_or("");
    let rest = rest.strip_prefix(r"\[?").unwrap_or(rest);
    let starts_with_ts = rest.starts_with("(?P<ts>") || rest.starts_with("(?<ts>");
    let names: Vec<&str> = columns.iter().map(|(n, _)| n.as_str()).collect();
    let level_ok = match names.iter().position(|n| *n == "level") {
        None => true,
        Some(i) => i == 1,
    };
    starts_with_ts && ts_format.is_empty() && level_ok && names.first() == Some(&"ts")
}

fn find_column(columns: &[String], names: &[&str]) -> Option<usize> {
    names
        .iter()
        .find_map(|n| columns.iter().position(|c| c.eq_ignore_ascii_case(n)))
}

/// Keys of the JSON objects in `sample`, in order of first appearance.
fn json_keys(sample: &[&[u8]]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in sample {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(line.trim_ascii()) {
            for key in map.keys() {
                if !keys.contains(key) && keys.len() < MAX_JSON_COLUMNS {
                    keys.push(key.clone());
                }
            }
        }
    }
    keys
}

#[derive(Debug)]
enum Inner {
    /// Column name and capture group index.
    Regex {
        regex: Regex,
        columns: Vec<(String, usize)>,
        fast_meta: bool,
    },
    Json {
        keys: Vec<String>,
    },
    Plain,
}

/// Timestamp and level of a record's first line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineMeta {
    pub ts: Option<Millis>,
    pub level: Option<Level>,
}

/// A compiled format; cheap to share between threads.
#[derive(Debug)]
pub struct CompiledFormat {
    pub name: String,
    inner: Inner,
    columns: Vec<String>,
    ts_column: Option<usize>,
    level_column: Option<usize>,
    timestamp_format: String,
    year: i64,
    level_word: Regex,
}

impl CompiledFormat {
    /// Field names (empty for plain text).
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    pub fn is_plain(&self) -> bool {
        matches!(self.inner, Inner::Plain)
    }

    pub fn is_json(&self) -> bool {
        matches!(self.inner, Inner::Json { .. })
    }

    /// Whether `line` starts a record (matches the format).
    pub fn is_header(&self, line: &[u8]) -> bool {
        match &self.inner {
            Inner::Regex { regex, .. } => regex.is_match(line),
            // (fast_meta does not change header detection)
            Inner::Json { .. } => json_object(line).is_some(),
            Inner::Plain => true,
        }
    }

    /// Field values of `line` (one per column), or `None` for a
    /// continuation line.
    pub fn fields<'a>(&self, line: &'a [u8]) -> Option<Vec<Option<Cow<'a, [u8]>>>> {
        match &self.inner {
            Inner::Regex { regex, columns, .. } => {
                let caps = regex.captures(line)?;
                Some(
                    columns
                        .iter()
                        .map(|(_, i)| caps.get(*i).map(|m| Cow::Borrowed(m.as_bytes())))
                        .collect(),
                )
            }
            Inner::Json { keys } => {
                let map = json_object(line)?;
                Some(
                    keys.iter()
                        .map(|k| map.get(k).map(|v| Cow::Owned(json_text(v).into_bytes())))
                        .collect(),
                )
            }
            Inner::Plain => Some(Vec::new()),
        }
    }

    /// Timestamp and level of `line`, or `None` for a continuation line.
    pub fn meta(&self, line: &[u8]) -> Option<LineMeta> {
        match &self.inner {
            Inner::Regex {
                regex,
                fast_meta: true,
                ..
            } => {
                if !regex.is_match(line) {
                    return None;
                }
                Some(LineMeta {
                    ts: time::parse_auto(&line[..line.len().min(64)], self.year),
                    level: self.level_word(line),
                })
            }
            Inner::Regex { regex, columns, .. } => {
                let caps = regex.captures(line)?;
                let field = |col: Option<usize>| {
                    col.and_then(|c| caps.get(columns[c].1))
                        .map(|m| m.as_bytes())
                };
                let ts = field(self.ts_column).and_then(|t| self.parse_ts(t));
                let level = match self.level_column {
                    Some(_) => field(self.level_column).and_then(Level::parse),
                    None => self.level_word(line),
                };
                Some(LineMeta { ts, level })
            }
            Inner::Json { keys } => {
                let map = json_object(line)?;
                let value = |col: Option<usize>| col.and_then(|c| map.get(&keys[c]));
                let ts = value(self.ts_column).and_then(|v| match v {
                    // Unix time: seconds, or milliseconds for large values.
                    serde_json::Value::Number(n) => n.as_f64().map(|f| {
                        if f.abs() > 1e11 {
                            f as Millis
                        } else {
                            (f * 1000.0) as Millis
                        }
                    }),
                    serde_json::Value::String(s) => self.parse_ts(s.as_bytes()),
                    _ => None,
                });
                let level = value(self.level_column).and_then(|v| match v {
                    serde_json::Value::String(s) => Level::parse(s.as_bytes()),
                    _ => None,
                });
                Some(LineMeta { ts, level })
            }
            Inner::Plain => Some(LineMeta {
                ts: time::parse_auto(&line[..line.len().min(64)], self.year),
                level: self.level_word(line),
            }),
        }
    }

    fn parse_ts(&self, text: &[u8]) -> Option<Millis> {
        if self.timestamp_format.is_empty() {
            time::parse_auto(text, self.year)
        } else {
            time::parse_with_format(std::str::from_utf8(text).ok()?, &self.timestamp_format)
        }
    }

    fn level_word(&self, line: &[u8]) -> Option<Level> {
        let head = &line[..line.len().min(LEVEL_SCAN_BYTES)];
        self.level_word
            .find(head)
            .and_then(|m| Level::parse(m.as_bytes()))
    }

    /// Pretty-printed JSON for JSON lines.
    pub fn pretty(&self, line: &[u8]) -> Option<String> {
        if !self.is_json() {
            return None;
        }
        let value: serde_json::Value = serde_json::from_slice(line.trim_ascii()).ok()?;
        serde_json::to_string_pretty(&value).ok()
    }

    /// Fraction of non-empty lines in `sample` that are record headers.
    pub fn match_ratio(&self, sample: &[&[u8]]) -> f32 {
        let lines: Vec<&&[u8]> = sample
            .iter()
            .filter(|l| !l.trim_ascii().is_empty())
            .collect();
        if lines.is_empty() {
            return 0.0;
        }
        let matched = lines.iter().filter(|l| self.is_header(l)).count();
        matched as f32 / lines.len() as f32
    }
}

fn json_object(line: &[u8]) -> Option<serde_json::Map<String, serde_json::Value>> {
    let line = line.trim_ascii();
    if line.first() != Some(&b'{') {
        return None;
    }
    match serde_json::from_slice(line) {
        Ok(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Result of [`detect`].
#[derive(Clone, Debug)]
pub struct Detection {
    pub format: LogFormat,
    /// Fraction of sample lines matching the format.
    pub ratio: f32,
}

/// Picks the built-in format that fits `sample` best. JSON lines win when at
/// least 60% of lines are JSON objects. Among regex formats the most specific
/// one (earliest in [`LogFormat::builtins`]) whose match ratio is within 90%
/// of the best is chosen; below 30% the file is treated as plain text.
pub fn detect(sample: &[&[u8]]) -> Detection {
    let builtins = LogFormat::builtins();
    let json = LogFormat::json()
        .compile(sample)
        .expect("json format compiles");
    let json_ratio = json.match_ratio(sample);
    if json_ratio >= 0.6 {
        return Detection {
            format: LogFormat::json(),
            ratio: json_ratio,
        };
    }
    let scored: Vec<(LogFormat, f32)> = builtins
        .into_iter()
        .filter(|f| matches!(f.kind, FormatKind::Regex(_)))
        .map(|f| {
            let ratio = f.compile(sample).map_or(0.0, |c| c.match_ratio(sample));
            (f, ratio)
        })
        .collect();
    let best = scored.iter().map(|(_, r)| *r).fold(0.0, f32::max);
    if best < 0.3 {
        return Detection {
            format: LogFormat::plain(),
            ratio: 1.0,
        };
    }
    let (format, ratio) = scored
        .into_iter()
        .find(|(_, r)| *r >= best * 0.9)
        .expect("best exists");
    Detection { format, ratio }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Millis = 1_767_225_600_000;

    fn lines(text: &str) -> Vec<&[u8]> {
        text.lines().map(str::as_bytes).collect()
    }

    fn text(v: &Option<Cow<'_, [u8]>>) -> String {
        v.as_ref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }

    const APP_LOG: &str = "\
2026-01-01 00:00:00.000 INFO  [worker-1] app.http - GET / 200
2026-01-01 00:00:01.500 ERROR [worker-2] app.db - connection reset
\tat com.example.Db.query(Db.java:10)
\tat com.example.Main.run(Main.java:5)
2026-01-01 00:00:02.000 WARN  [worker-1] app.cache - slow";

    #[test]
    fn detects_app_format_and_parses_fields() {
        let sample = lines(APP_LOG);
        let d = detect(&sample);
        assert!(
            d.format.name.starts_with("Czas + poziom"),
            "{}",
            d.format.name
        );
        let f = d.format.compile(&sample).unwrap();
        assert_eq!(f.columns(), ["ts", "level", "thread", "logger", "msg"]);
        let fields = f.fields(sample[1]).unwrap();
        assert_eq!(text(&fields[1]), "ERROR");
        assert_eq!(text(&fields[2]), "worker-2");
        assert_eq!(text(&fields[3]), "app.db");
        assert_eq!(text(&fields[4]), "connection reset");
        assert!(
            f.fields(sample[2]).is_none(),
            "stack trace is a continuation"
        );
        assert_eq!(
            f.meta(sample[1]),
            Some(LineMeta {
                ts: Some(T0 + 1500),
                level: Some(Level::Error)
            })
        );
        assert_eq!(f.meta(sample[2]), None);
    }

    #[test]
    fn detects_json_lines() {
        let sample = lines(
            r#"{"time":"2026-01-01T00:00:00Z","level":"warn","msg":"a","n":1}
{"time":1767225601,"level":"error","msg":"b","extra":{"k":true}}"#,
        );
        let d = detect(&sample);
        assert_eq!(d.format.kind, FormatKind::Json);
        let f = d.format.compile(&sample).unwrap();
        assert_eq!(f.columns(), ["time", "level", "msg", "n", "extra"]);
        assert_eq!(
            f.meta(sample[0]),
            Some(LineMeta {
                ts: Some(T0),
                level: Some(Level::Warn)
            })
        );
        assert_eq!(f.meta(sample[1]).unwrap().ts, Some(T0 + 1000));
        let fields = f.fields(sample[1]).unwrap();
        assert_eq!(text(&fields[4]), r#"{"k":true}"#);
        assert!(f.pretty(sample[1]).unwrap().contains("\n"));
        assert!(f.meta(b"not json").is_none());
    }

    #[test]
    fn detects_syslog() {
        let sample = lines(
            "Jan  1 00:00:00 host sshd[123]: Accepted password\nJan  1 00:00:05 host cron: job ERROR",
        );
        let d = detect(&sample);
        assert_eq!(d.format.name, "Syslog");
        let f = d.format.compile(&sample).unwrap();
        let meta = f.meta(sample[1]).unwrap();
        assert_eq!(meta.level, Some(Level::Error), "level word fallback");
        assert!(meta.ts.is_some());
    }

    #[test]
    fn detects_apache() {
        let sample = lines(
            r#"127.0.0.1 - - [01/Jan/2026:00:00:00 +0000] "GET / HTTP/1.1" 200 512 "-" "curl""#,
        );
        let d = detect(&sample);
        assert_eq!(d.format.name, "Apache/nginx (combined)");
        let f = d.format.compile(&sample).unwrap();
        assert_eq!(f.meta(sample[0]).unwrap().ts, Some(T0));
    }

    #[test]
    fn unstructured_text_is_plain() {
        let sample = lines("hello\nworld ERROR here\nmore text");
        let d = detect(&sample);
        assert_eq!(d.format.kind, FormatKind::Plain);
        let f = d.format.compile(&sample).unwrap();
        assert!(f.is_header(b"anything"));
        assert_eq!(f.meta(sample[1]).unwrap().level, Some(Level::Error));
        assert_eq!(f.meta(b"2026-01-01 00:00:00 x").unwrap().ts, Some(T0));
    }

    #[test]
    fn custom_format_with_timestamp_format() {
        let format = LogFormat {
            name: "custom".into(),
            kind: FormatKind::Regex(r"^(?P<ts>\S+ \S+) (?P<level>\w+): (?P<msg>.*)$".into()),
            timestamp_format: "%d.%m.%Y %H:%M:%S".into(),
        };
        let f = format.compile(&[]).unwrap();
        let meta = f.meta(b"01.01.2026 00:00:10 warning: disk").unwrap();
        assert_eq!(
            meta,
            LineMeta {
                ts: Some(T0 + 10_000),
                level: Some(Level::Warn)
            }
        );
    }

    #[test]
    fn fast_meta_matches_capture_groups() {
        let sample = lines(APP_LOG);
        for format in LogFormat::builtins() {
            let FormatKind::Regex(pattern) = &format.kind else {
                continue;
            };
            let fast = format.compile(&sample).unwrap();
            // `(?:)` after `^` disables the fast path without changing matches.
            let slow_format = LogFormat::regex("slow", &pattern.replacen('^', "^(?:)", 1));
            let slow = slow_format.compile(&sample).unwrap();
            for line in &sample {
                assert_eq!(
                    fast.meta(line),
                    slow.meta(line),
                    "{}: {:?}",
                    format.name,
                    String::from_utf8_lossy(line)
                );
            }
        }
        let first = LogFormat::builtins()[0].compile(&sample).unwrap();
        assert!(matches!(
            first.inner,
            Inner::Regex {
                fast_meta: true,
                ..
            }
        ));
    }

    #[test]
    fn invalid_custom_formats() {
        assert!(LogFormat::regex("x", "(unclosed").compile(&[]).is_err());
        assert!(
            LogFormat::regex("x", r"^\d+ .*$").compile(&[]).is_err(),
            "no named groups"
        );
        let mut bad_ts = LogFormat::regex("x", r"(?P<ts>.*)");
        bad_ts.timestamp_format = "%Q".into();
        assert!(bad_ts.compile(&[]).is_err());
    }
}
