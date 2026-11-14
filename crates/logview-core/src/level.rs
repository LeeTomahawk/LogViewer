//! Log levels.

/// Log level of a line or record. The numeric value is the packed code
/// (0 is reserved for "no level").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    Trace = 1,
    Debug = 2,
    Info = 3,
    Warn = 4,
    Error = 5,
    Fatal = 6,
}

impl Level {
    pub const ALL: [Level; 6] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.get(code.checked_sub(1)? as usize).copied()
    }

    /// Parses a level name (case-insensitive), including common synonyms and
    /// abbreviations (`WARNING`, `ERR`, `CRIT`, `SEVERE`, `FINE`, …).
    pub fn parse(text: &[u8]) -> Option<Self> {
        let text = text.trim_ascii();
        if text.len() > 11 {
            return None;
        }
        let mut upper = [0u8; 11];
        let upper = &mut upper[..text.len()];
        for (dst, src) in upper.iter_mut().zip(text) {
            *dst = src.to_ascii_uppercase();
        }
        Some(match &*upper {
            b"TRACE" | b"TRC" | b"FINER" | b"FINEST" | b"VERBOSE" => Level::Trace,
            b"DEBUG" | b"DBG" | b"FINE" => Level::Debug,
            b"INFO" | b"INF" | b"INFORMATION" | b"NOTICE" | b"CONFIG" => Level::Info,
            b"WARN" | b"WARNING" | b"WRN" => Level::Warn,
            b"ERROR" | b"ERR" | b"SEVERE" => Level::Error,
            b"FATAL" | b"CRITICAL" | b"CRIT" | b"EMERG" | b"EMERGENCY" | b"ALERT" | b"PANIC" => {
                Level::Fatal
            }
            _ => return None,
        })
    }
}

/// A set of levels plus "no level", as a bit mask: bit 0 = no level,
/// bit `n` = level with code `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LevelSet(pub u8);

impl LevelSet {
    pub const ALL: LevelSet = LevelSet(0b0111_1111);

    pub fn contains(self, level: Option<Level>) -> bool {
        self.0 & Self::bit(level) != 0
    }

    pub fn set(&mut self, level: Option<Level>, on: bool) {
        if on {
            self.0 |= Self::bit(level);
        } else {
            self.0 &= !Self::bit(level);
        }
    }

    pub fn is_all(self) -> bool {
        self.0 & Self::ALL.0 == Self::ALL.0
    }

    fn bit(level: Option<Level>) -> u8 {
        1 << level.map_or(0, Level::code)
    }
}

impl Default for LevelSet {
    fn default() -> Self {
        Self::ALL
    }
}

/// In profiles a level set is a list of names, e.g. `["ERROR", "WARN", "none"]`.
impl serde::Serialize for LevelSet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let all = Level::ALL
            .iter()
            .map(|l| Some(*l))
            .chain(std::iter::once(None));
        let names: Vec<&str> = all
            .filter(|l| self.contains(*l))
            .map(|l| l.map_or("none", Level::name))
            .collect();
        let mut seq = s.serialize_seq(Some(names.len()))?;
        for n in names {
            seq.serialize_element(n)?;
        }
        seq.end()
    }
}

impl<'de> serde::Deserialize<'de> for LevelSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(d)?;
        let mut set = LevelSet(0);
        for name in names {
            if name.eq_ignore_ascii_case("none") {
                set.set(None, true);
            } else {
                let level = Level::parse(name.as_bytes())
                    .ok_or_else(|| serde::de::Error::custom(format!("nieznany poziom {name:?}")))?;
                set.set(Some(level), true);
            }
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_synonyms() {
        assert_eq!(Level::parse(b"warn"), Some(Level::Warn));
        assert_eq!(Level::parse(b" WARNING "), Some(Level::Warn));
        assert_eq!(Level::parse(b"Err"), Some(Level::Error));
        assert_eq!(Level::parse(b"CRITICAL"), Some(Level::Fatal));
        assert_eq!(Level::parse(b"notice"), Some(Level::Info));
        assert_eq!(Level::parse(b"hello"), None);
        assert_eq!(Level::parse(b"INFORMATIONAL"), None);
    }

    #[test]
    fn codes_round_trip() {
        for level in Level::ALL {
            assert_eq!(Level::from_code(level.code()), Some(level));
        }
        assert_eq!(Level::from_code(0), None);
        assert_eq!(Level::from_code(7), None);
    }

    #[test]
    fn level_set() {
        let mut set = LevelSet::ALL;
        assert!(set.is_all());
        set.set(Some(Level::Debug), false);
        set.set(None, false);
        assert!(!set.contains(Some(Level::Debug)));
        assert!(!set.contains(None));
        assert!(set.contains(Some(Level::Error)));
        assert!(!set.is_all());
    }
}
