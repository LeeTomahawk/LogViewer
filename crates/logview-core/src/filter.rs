//! Filter rules and line matching.
//!
//! A [`FilterSpec`] is an ordered list of [`FilterRule`]s plus the include
//! mode and the number of context lines. [`FilterSpec::compile`] turns it into
//! a [`CompiledFilter`] which decides whether a line passes:
//!
//! * a line matching any enabled **exclude** rule is rejected;
//! * otherwise, if there are no include rules, the line passes;
//! * otherwise it must match **all** include rules ([`IncludeMode::All`]) or
//!   **any** of them ([`IncludeMode::Any`]).
//!
//! Rules that are disabled or have an empty pattern are ignored.
//!
//! The spec may also restrict records by level ([`FilterSpec::levels`]) and
//! by time ([`FilterSpec::time_from`], [`FilterSpec::time_to`]); those need
//! record metadata from a [`crate::records::ParseJob`]. Records without a
//! timestamp are rejected while a time range is set.
//!
//! For multi-line records, text rules are evaluated over all lines of the
//! record: an include rule matches if any line matches it, and the record is
//! excluded if any line matches an exclude rule.

use crate::level::{Level, LevelSet};
use crate::pattern::Pattern;
pub use crate::pattern::PatternKind;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FilterRule {
    pub pattern: String,
    pub kind: PatternKind,
    pub case_sensitive: bool,
    /// Reject lines matching this rule instead of requiring a match.
    pub exclude: bool,
    pub enabled: bool,
}

impl Default for FilterRule {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            kind: PatternKind::Text,
            case_sensitive: false,
            exclude: false,
            enabled: true,
        }
    }
}

impl FilterRule {
    pub fn include(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_owned(),
            ..Self::default()
        }
    }

    pub fn exclude(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_owned(),
            exclude: true,
            ..Self::default()
        }
    }

    pub fn regex(mut self) -> Self {
        self.kind = PatternKind::Regex;
        self
    }

    pub fn case_sensitive(mut self) -> Self {
        self.case_sensitive = true;
        self
    }

    /// Whether the rule takes part in filtering.
    pub fn is_active(&self) -> bool {
        self.enabled && !self.pattern.is_empty()
    }
}

/// How include rules are combined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IncludeMode {
    /// Every include rule must match (AND).
    #[default]
    All,
    /// At least one include rule must match (OR).
    Any,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FilterSpec {
    pub rules: Vec<FilterRule>,
    pub mode: IncludeMode,
    /// Context lines shown before each match.
    pub context_before: usize,
    /// Context lines shown after each match.
    pub context_after: usize,
    /// Levels to keep (all = no level filtering).
    pub levels: LevelSet,
    /// Keep records at or after this time (seconds).
    #[serde(skip)]
    pub time_from: Option<i64>,
    /// Keep records at or before this time (seconds).
    #[serde(skip)]
    pub time_to: Option<i64>,
}

/// Maximum number of active include rules (tracked in a 64-bit mask for
/// multi-line records).
pub const MAX_INCLUDES: usize = 64;

/// A rule that failed to compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleError {
    /// Index of the rule in [`FilterSpec::rules`].
    pub rule: usize,
    pub message: String,
}

impl FilterSpec {
    /// Whether any rule is active, i.e. whether the filter changes anything.
    pub fn is_active(&self) -> bool {
        self.rules.iter().any(FilterRule::is_active)
            || !self.levels.is_all()
            || self.time_from.is_some()
            || self.time_to.is_some()
    }

    /// Compiles the active rules. Returns all compile errors at once so the UI
    /// can show each next to its rule.
    pub fn compile(&self) -> Result<CompiledFilter, Vec<RuleError>> {
        let mut includes = Vec::new();
        let mut excludes = Vec::new();
        let mut errors = Vec::new();
        for (i, rule) in self.rules.iter().enumerate() {
            if !rule.is_active() {
                continue;
            }
            match Pattern::new(&rule.pattern, rule.kind, rule.case_sensitive) {
                Ok(m) if rule.exclude => excludes.push(m),
                Ok(_) if includes.len() == MAX_INCLUDES => errors.push(RuleError {
                    rule: i,
                    message: format!("maksymalnie {MAX_INCLUDES} aktywnych filtrów „+”"),
                }),
                Ok(m) => includes.push(m),
                Err(message) => errors.push(RuleError { rule: i, message }),
            }
        }
        if errors.is_empty() {
            Ok(CompiledFilter {
                includes,
                excludes,
                mode: self.mode,
                levels: self.levels,
                time_from: self.time_from,
                time_to: self.time_to,
            })
        } else {
            Err(errors)
        }
    }
}

/// A compiled, immutable filter; cheap to share between threads.
#[derive(Clone, Debug)]
pub struct CompiledFilter {
    includes: Vec<Pattern>,
    excludes: Vec<Pattern>,
    mode: IncludeMode,
    levels: LevelSet,
    time_from: Option<i64>,
    time_to: Option<i64>,
}

impl CompiledFilter {
    /// Whether the filter has any rule; an empty filter passes every line.
    pub fn is_active(&self) -> bool {
        self.has_text_rules() || self.needs_meta()
    }

    pub fn has_text_rules(&self) -> bool {
        !self.includes.is_empty() || !self.excludes.is_empty()
    }

    /// Whether the filter uses record level/time.
    pub fn needs_meta(&self) -> bool {
        !self.levels.is_all() || self.time_from.is_some() || self.time_to.is_some()
    }

    /// Level/time part of the filter (`ts` in seconds).
    pub fn matches_meta(&self, ts: Option<i64>, level: Option<Level>) -> bool {
        if !self.levels.contains(level) {
            return false;
        }
        if self.time_from.is_none() && self.time_to.is_none() {
            return true;
        }
        let Some(ts) = ts else { return false };
        self.time_from.is_none_or(|from| ts >= from) && self.time_to.is_none_or(|to| ts <= to)
    }

    /// Text rules over all lines of a record.
    pub fn matches_record<'a>(&self, lines: impl IntoIterator<Item = &'a [u8]>) -> bool {
        if !self.has_text_rules() {
            return true;
        }
        // Bit i set = include rule i matched some line ([`MAX_INCLUDES`]).
        let mut matched: u64 = 0;
        for line in lines {
            if self.excludes.iter().any(|m| m.is_match(line)) {
                return false;
            }
            for (i, m) in self.includes.iter().enumerate() {
                if matched & (1 << i) == 0 && m.is_match(line) {
                    matched |= 1 << i;
                }
            }
        }
        if self.includes.is_empty() {
            return true;
        }
        let all_bits = u64::MAX >> (64 - self.includes.len());
        match self.mode {
            IncludeMode::All => matched == all_bits,
            IncludeMode::Any => matched != 0,
        }
    }

    pub fn matches(&self, line: &[u8]) -> bool {
        if self.excludes.iter().any(|m| m.is_match(line)) {
            return false;
        }
        if self.includes.is_empty() {
            return true;
        }
        match self.mode {
            IncludeMode::All => self.includes.iter().all(|m| m.is_match(line)),
            IncludeMode::Any => self.includes.iter().any(|m| m.is_match(line)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(rules: Vec<FilterRule>, mode: IncludeMode) -> CompiledFilter {
        FilterSpec {
            rules,
            mode,
            ..Default::default()
        }
        .compile()
        .unwrap()
    }

    #[test]
    fn empty_spec_passes_everything() {
        let f = spec(vec![], IncludeMode::All);
        assert!(!f.is_active());
        assert!(f.matches(b"anything"));
    }

    #[test]
    fn text_is_case_insensitive_by_default() {
        let f = spec(vec![FilterRule::include("error")], IncludeMode::All);
        assert!(f.matches(b"2026 ERROR boom"));
        assert!(!f.matches(b"2026 INFO ok"));
    }

    #[test]
    fn case_sensitive_text() {
        let f = spec(
            vec![FilterRule::include("Error").case_sensitive()],
            IncludeMode::All,
        );
        assert!(f.matches(b"an Error"));
        assert!(!f.matches(b"an error"));
    }

    #[test]
    fn text_special_chars_are_literal() {
        let f = spec(vec![FilterRule::include("a.b(")], IncludeMode::All);
        assert!(f.matches(b"x a.b( y"));
        assert!(!f.matches(b"axb("));
    }

    #[test]
    fn case_insensitive_polish_text() {
        let f = spec(vec![FilterRule::include("ZAŻÓŁĆ")], IncludeMode::All);
        assert!(f.matches("user=zażółć".as_bytes()));
    }

    #[test]
    fn regex_rule() {
        let f = spec(
            vec![FilterRule::include(r"took \d{4,} ms").regex()],
            IncludeMode::All,
        );
        assert!(f.matches(b"Slow query took 1234 ms"));
        assert!(!f.matches(b"Slow query took 999 ms"));
    }

    #[test]
    fn regex_works_on_invalid_utf8() {
        let f = spec(vec![FilterRule::include("b+").regex()], IncludeMode::All);
        assert!(f.matches(b"\xFF\xFEbb"));
    }

    #[test]
    fn all_vs_any() {
        let rules = vec![FilterRule::include("db"), FilterRule::include("warn")];
        let all = spec(rules.clone(), IncludeMode::All);
        let any = spec(rules, IncludeMode::Any);
        assert!(all.matches(b"WARN app.db slow"));
        assert!(!all.matches(b"INFO app.db ok"));
        assert!(any.matches(b"INFO app.db ok"));
        assert!(any.matches(b"WARN app.http"));
        assert!(!any.matches(b"INFO app.http"));
    }

    #[test]
    fn exclude_wins_over_include() {
        let f = spec(
            vec![
                FilterRule::include("error"),
                FilterRule::exclude("heartbeat"),
            ],
            IncludeMode::Any,
        );
        assert!(f.matches(b"ERROR real problem"));
        assert!(!f.matches(b"ERROR heartbeat failed"));
    }

    #[test]
    fn exclude_only() {
        let f = spec(vec![FilterRule::exclude("DEBUG")], IncludeMode::All);
        assert!(f.matches(b"INFO x"));
        assert!(!f.matches(b"DEBUG x"));
    }

    #[test]
    fn disabled_and_empty_rules_are_ignored() {
        let mut disabled = FilterRule::include("nothing-matches-this");
        disabled.enabled = false;
        let spec = FilterSpec {
            rules: vec![disabled, FilterRule::include("")],
            ..Default::default()
        };
        assert!(!spec.is_active());
        assert!(spec.compile().unwrap().matches(b"line"));
    }

    #[test]
    fn invalid_regex_reports_rule_index() {
        let spec = FilterSpec {
            rules: vec![
                FilterRule::include("ok"),
                FilterRule::include("(unclosed").regex(),
                FilterRule::include("[z-a]").regex(),
            ],
            ..Default::default()
        };
        let errors = spec.compile().unwrap_err();
        let rules: Vec<usize> = errors.iter().map(|e| e.rule).collect();
        assert_eq!(rules, vec![1, 2]);
        assert!(!errors[0].message.is_empty());
    }
}

#[cfg(test)]
mod meta_tests {
    use super::*;

    fn compile(spec: FilterSpec) -> CompiledFilter {
        spec.compile().unwrap()
    }

    #[test]
    fn level_filter() {
        let mut levels = LevelSet::ALL;
        levels.set(Some(Level::Debug), false);
        levels.set(None, false);
        let f = compile(FilterSpec {
            levels,
            ..Default::default()
        });
        assert!(f.is_active() && f.needs_meta() && !f.has_text_rules());
        assert!(f.matches_meta(None, Some(Level::Error)));
        assert!(!f.matches_meta(None, Some(Level::Debug)));
        assert!(!f.matches_meta(None, None));
    }

    #[test]
    fn time_range_is_inclusive_and_needs_timestamp() {
        let f = compile(FilterSpec {
            time_from: Some(100),
            time_to: Some(200),
            ..Default::default()
        });
        assert!(f.matches_meta(Some(100), None));
        assert!(f.matches_meta(Some(200), None));
        assert!(!f.matches_meta(Some(99), None));
        assert!(!f.matches_meta(Some(201), None));
        assert!(!f.matches_meta(None, None));
        let open = compile(FilterSpec {
            time_from: Some(100),
            ..Default::default()
        });
        assert!(open.matches_meta(Some(i64::MAX), None));
    }

    #[test]
    fn record_matching_spans_lines() {
        let lines: [&[u8]; 3] = [b"ERROR request failed", b"\tat Db.query", b"\tat Main.run"];
        let all = compile(FilterSpec {
            rules: vec![
                FilterRule::include("error"),
                FilterRule::include("Db.query"),
            ],
            ..Default::default()
        });
        assert!(
            all.matches_record(lines),
            "rules matched on different lines"
        );
        assert!(!all.matches_record([&b"ERROR only"[..]]));
        let excl = compile(FilterSpec {
            rules: vec![
                FilterRule::include("error"),
                FilterRule::exclude("Main.run"),
            ],
            ..Default::default()
        });
        assert!(
            !excl.matches_record(lines),
            "exclude on a continuation line"
        );
        let any = compile(FilterSpec {
            rules: vec![FilterRule::include("nothing"), FilterRule::include("Main")],
            mode: IncludeMode::Any,
            ..Default::default()
        });
        assert!(any.matches_record(lines));
    }

    #[test]
    fn too_many_includes() {
        let rules = (0..MAX_INCLUDES + 1)
            .map(|i| FilterRule::include(&format!("p{i}")))
            .collect();
        let errors = FilterSpec {
            rules,
            ..Default::default()
        }
        .compile()
        .unwrap_err();
        assert_eq!(errors[0].rule, MAX_INCLUDES);
    }
}
