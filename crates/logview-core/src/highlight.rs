//! Highlight rules: regex/text → colors, for a matched fragment or a whole line.
//!
//! Rules are ordered by priority (first = highest). For fragments, where
//! matches of several rules overlap, the higher-priority rule wins byte by
//! byte. For whole lines, the first matching line rule sets the line style;
//! fragment styles are drawn on top of it.

use std::ops::Range;

use crate::filter::RuleError;
use crate::pattern::{Pattern, PatternKind};

/// RGBA color, not premultiplied.
pub type Rgba = [u8; 4];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Only the matched text is styled.
    #[default]
    Match,
    /// The whole line is styled when the pattern matches anywhere in it.
    Line,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Style {
    #[serde(with = "color_hex", skip_serializing_if = "Option::is_none")]
    pub fg: Option<Rgba>,
    #[serde(with = "color_hex", skip_serializing_if = "Option::is_none")]
    pub bg: Option<Rgba>,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HighlightRule {
    pub pattern: String,
    pub kind: PatternKind,
    pub case_sensitive: bool,
    pub scope: Scope,
    pub style: Style,
    pub enabled: bool,
    /// Show lines matching this rule on the scrollbar marker bar.
    pub minimap: bool,
}

impl Default for HighlightRule {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            kind: PatternKind::Text,
            case_sensitive: false,
            scope: Scope::Match,
            style: Style {
                fg: Some([0xFF, 0x52, 0x52, 0xFF]),
                ..Style::default()
            },
            enabled: true,
            minimap: false,
        }
    }
}

impl HighlightRule {
    pub fn is_active(&self) -> bool {
        self.enabled && !self.pattern.is_empty()
    }

    /// Color used for this rule on the marker bar: background if set,
    /// otherwise the text color.
    pub fn marker_color(&self) -> Option<Rgba> {
        self.style
            .bg
            .map(|[r, g, b, _]| [r, g, b, 0xFF])
            .or(self.style.fg)
    }

    /// Default rules for log levels.
    pub fn defaults() -> Vec<Self> {
        let level = |pattern: &str, fg: Rgba, bold: bool, minimap: bool| Self {
            pattern: pattern.to_owned(),
            kind: PatternKind::Regex,
            case_sensitive: true,
            scope: Scope::Match,
            style: Style {
                fg: Some(fg),
                bg: None,
                bold,
                italic: false,
            },
            enabled: true,
            minimap,
        };
        vec![
            level(
                r"\b(ERROR|FATAL|CRITICAL|SEVERE)\b",
                [0xF4, 0x43, 0x36, 0xFF],
                true,
                true,
            ),
            level(r"\bWARN(ING)?\b", [0xFF, 0x98, 0x00, 0xFF], true, true),
            level(r"\bINFO\b", [0x4C, 0xAF, 0x50, 0xFF], false, false),
            level(r"\bDEBUG\b", [0x42, 0xA5, 0xF5, 0xFF], false, false),
            level(r"\bTRACE\b", [0x9E, 0x9E, 0x9E, 0xFF], false, false),
            Self {
                pattern: r"\b(ERROR|FATAL|CRITICAL|SEVERE)\b".to_owned(),
                kind: PatternKind::Regex,
                case_sensitive: true,
                scope: Scope::Line,
                style: Style {
                    fg: None,
                    bg: Some([0xF4, 0x43, 0x36, 0x28]),
                    bold: false,
                    italic: false,
                },
                enabled: true,
                minimap: false,
            },
        ]
    }
}

/// Style id used by [`LineHighlight`]: an index into the rule list, or one of
/// the extra ids passed to [`Highlighter::highlight`] (e.g. search hits).
pub type StyleId = u16;

/// Highlighting of one line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineHighlight {
    /// Rule styling the whole line, if any.
    pub line_rule: Option<StyleId>,
    /// Consecutive runs covering the whole input; `None` = unstyled.
    pub runs: Vec<(Range<usize>, Option<StyleId>)>,
}

#[derive(Debug)]
struct CompiledRule {
    pattern: Pattern,
    scope: Scope,
    id: StyleId,
    minimap: bool,
}

/// Compiled highlight rules; cheap to share between threads.
#[derive(Debug, Default)]
pub struct Highlighter {
    rules: Vec<CompiledRule>,
}

/// Bit used in [`Highlighter::marker_mask`] for rules with an index ≥ 63.
pub const MAX_MARKER_RULES: usize = 63;

impl Highlighter {
    /// Compiles active rules. Style ids are indices into `rules`.
    pub fn compile(rules: &[HighlightRule]) -> Result<Self, Vec<RuleError>> {
        let mut compiled = Vec::new();
        let mut errors = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            if !rule.is_active() {
                continue;
            }
            match Pattern::new(&rule.pattern, rule.kind, rule.case_sensitive) {
                Ok(pattern) => compiled.push(CompiledRule {
                    pattern,
                    scope: rule.scope,
                    id: i as StyleId,
                    minimap: rule.minimap && i < MAX_MARKER_RULES,
                }),
                Err(message) => errors.push(RuleError { rule: i, message }),
            }
        }
        if errors.is_empty() {
            Ok(Self { rules: compiled })
        } else {
            Err(errors)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether any rule is shown on the marker bar.
    pub fn has_markers(&self) -> bool {
        self.rules.iter().any(|r| r.minimap)
    }

    /// Highlights `hay`. `extra` patterns (e.g. the search) take priority over
    /// all rules, the last one being the strongest.
    pub fn highlight(&self, hay: &[u8], extra: &[(&Pattern, StyleId)]) -> LineHighlight {
        let line_rule = self
            .rules
            .iter()
            .find(|r| r.scope == Scope::Line && r.pattern.is_match(hay))
            .map(|r| r.id);

        // Paint from lowest to highest priority; later writes win.
        let mut owner: Vec<Option<StyleId>> = Vec::new();
        let mut paint = |pattern: &Pattern, id: StyleId| {
            for range in pattern.find_iter(hay) {
                if owner.is_empty() {
                    owner = vec![None; hay.len()];
                }
                owner[range].fill(Some(id));
            }
        };
        for rule in self.rules.iter().rev().filter(|r| r.scope == Scope::Match) {
            paint(&rule.pattern, rule.id);
        }
        for &(pattern, id) in extra {
            paint(pattern, id);
        }

        LineHighlight {
            line_rule,
            runs: if owner.is_empty() {
                if hay.is_empty() {
                    Vec::new()
                } else {
                    vec![(0..hay.len(), None)]
                }
            } else {
                compress(&owner)
            },
        }
    }

    /// Bit `i` is set if rule `i` (shown on the marker bar) matches `hay`.
    pub fn marker_mask(&self, hay: &[u8]) -> u64 {
        self.rules
            .iter()
            .filter(|r| r.minimap && r.pattern.is_match(hay))
            .fold(0, |mask, r| mask | (1 << r.id))
    }
}

fn compress(owner: &[Option<StyleId>]) -> Vec<(Range<usize>, Option<StyleId>)> {
    let mut runs: Vec<(Range<usize>, Option<StyleId>)> = Vec::new();
    for (i, &id) in owner.iter().enumerate() {
        match runs.last_mut() {
            Some((range, last)) if *last == id => range.end = i + 1,
            _ => runs.push((i..i + 1, id)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, scope: Scope) -> HighlightRule {
        HighlightRule {
            pattern: pattern.to_owned(),
            scope,
            ..HighlightRule::default()
        }
    }

    #[test]
    fn no_match_is_one_plain_run() {
        let h = Highlighter::compile(&[rule("zzz", Scope::Match)]).unwrap();
        let out = h.highlight(b"abc", &[]);
        assert_eq!(out.runs, vec![(0..3, None)]);
        assert_eq!(out.line_rule, None);
        assert!(h.highlight(b"", &[]).runs.is_empty());
    }

    #[test]
    fn match_runs_cover_line() {
        let h = Highlighter::compile(&[rule("b", Scope::Match)]).unwrap();
        let out = h.highlight(b"abcb", &[]);
        assert_eq!(
            out.runs,
            vec![(0..1, None), (1..2, Some(0)), (2..3, None), (3..4, Some(0))]
        );
    }

    #[test]
    fn higher_priority_wins_on_overlap() {
        // Rule 0 "bc" has priority over rule 1 "abcd".
        let h =
            Highlighter::compile(&[rule("bc", Scope::Match), rule("abcd", Scope::Match)]).unwrap();
        let out = h.highlight(b"abcd", &[]);
        assert_eq!(
            out.runs,
            vec![(0..1, Some(1)), (1..3, Some(0)), (3..4, Some(1))]
        );
    }

    #[test]
    fn extra_patterns_win() {
        let h = Highlighter::compile(&[rule("abc", Scope::Match)]).unwrap();
        let search = Pattern::new("b", PatternKind::Text, true).unwrap();
        let out = h.highlight(b"abc", &[(&search, 99)]);
        assert_eq!(
            out.runs,
            vec![(0..1, Some(0)), (1..2, Some(99)), (2..3, Some(0))]
        );
    }

    #[test]
    fn first_line_rule_wins() {
        let h = Highlighter::compile(&[
            rule("x", Scope::Match),
            rule("warn", Scope::Line),
            rule("a", Scope::Line),
        ])
        .unwrap();
        assert_eq!(h.highlight(b"warn a", &[]).line_rule, Some(1));
        assert_eq!(h.highlight(b"only a", &[]).line_rule, Some(2));
        assert_eq!(h.highlight(b"none", &[]).line_rule, None);
    }

    #[test]
    fn disabled_rules_keep_indices() {
        let mut off = rule("a", Scope::Match);
        off.enabled = false;
        let h = Highlighter::compile(&[off, rule("b", Scope::Match)]).unwrap();
        assert_eq!(
            h.highlight(b"ab", &[]).runs,
            vec![(0..1, None), (1..2, Some(1))]
        );
    }

    #[test]
    fn marker_mask_uses_rule_indices() {
        let mut a = rule("a", Scope::Match);
        a.minimap = true;
        let mut c = rule("c", Scope::Line);
        c.minimap = true;
        let h = Highlighter::compile(&[a, rule("b", Scope::Match), c]).unwrap();
        assert!(h.has_markers());
        assert_eq!(h.marker_mask(b"abc"), 0b101);
        assert_eq!(h.marker_mask(b"b"), 0);
    }

    #[test]
    fn invalid_rule_reports_index() {
        let mut bad = rule("(", Scope::Match);
        bad.kind = PatternKind::Regex;
        let errors = Highlighter::compile(&[rule("ok", Scope::Match), bad]).unwrap_err();
        assert_eq!(errors[0].rule, 1);
    }

    #[test]
    fn defaults_compile_and_match_levels() {
        let rules = HighlightRule::defaults();
        let h = Highlighter::compile(&rules).unwrap();
        let out = h.highlight(b"2026-01-01 ERROR [main] boom", &[]);
        assert!(out.line_rule.is_some());
        assert!(
            out.runs
                .iter()
                .any(|(r, id)| *r == (11..16) && *id == Some(0))
        );
        assert_eq!(h.marker_mask(b"x WARN y"), 0b10);
        assert_eq!(h.highlight(b"ERRORS", &[]).line_rule, None);
    }
}

/// Colors as `#RRGGBB` (opaque) or `#RRGGBBAA` in profiles.
pub mod color_hex {
    use super::Rgba;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn to_hex([r, g, b, a]: Rgba) -> String {
        if a == 0xFF {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
        }
    }

    pub fn from_hex(s: &str) -> Option<Rgba> {
        let hex = s.trim().strip_prefix('#')?;
        if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let a = if hex.len() == 8 { byte(6)? } else { 0xFF };
        Some([byte(0)?, byte(2)?, byte(4)?, a])
    }

    pub fn serialize<S: Serializer>(value: &Option<Rgba>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(c) => s.serialize_str(&to_hex(*c)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Rgba>, D::Error> {
        let text = Option::<String>::deserialize(d)?;
        match text {
            None => Ok(None),
            Some(t) if t.is_empty() => Ok(None),
            Some(t) => from_hex(&t).map(Some).ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "nieprawidłowy kolor {t:?}, użyj #RRGGBB lub #RRGGBBAA"
                ))
            }),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hex_round_trip() {
            for c in [[0, 0, 0, 255], [0xF4, 0x43, 0x36, 0x28], [1, 2, 3, 4]] {
                assert_eq!(from_hex(&to_hex(c)), Some(c));
            }
            assert_eq!(to_hex([0xAB, 0xCD, 0xEF, 0xFF]), "#ABCDEF");
            assert_eq!(from_hex("#abcdef"), Some([0xAB, 0xCD, 0xEF, 0xFF]));
            assert_eq!(from_hex("abcdef"), None);
            assert_eq!(from_hex("#abc"), None);
            assert_eq!(from_hex("#zzzzzz"), None);
        }
    }
}
