//! A compiled text or regex pattern, shared by filters, highlighting and search.

use std::ops::Range;

use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatternKind {
    /// Plain substring.
    #[default]
    Text,
    /// Regular expression (`regex` crate syntax).
    Regex,
}

/// Limit for the compiled regex size; protects against patterns that would
/// take huge amounts of memory.
const REGEX_SIZE_LIMIT: usize = 64 << 20;

#[derive(Clone, Debug)]
enum Inner {
    /// Case-sensitive substring: SIMD substring search.
    Text(Box<memmem::Finder<'static>>),
    /// Regex, or case-insensitive text (escaped into a regex).
    Regex(Regex),
}

/// A compiled pattern matching raw line bytes (invalid UTF-8 is fine).
#[derive(Clone, Debug)]
pub struct Pattern(Inner);

impl Pattern {
    /// Compiles `pattern`. Text patterns match literally; case-insensitive
    /// matching is Unicode-aware (`ą` matches `Ą`). The error is a
    /// human-readable message (for regex syntax errors, with a caret).
    pub fn new(pattern: &str, kind: PatternKind, case_sensitive: bool) -> Result<Self, String> {
        let source = match (kind, case_sensitive) {
            (PatternKind::Text, true) => {
                return Ok(Self(Inner::Text(Box::new(
                    memmem::Finder::new(pattern.as_bytes()).into_owned(),
                ))));
            }
            (PatternKind::Text, false) => regex::escape(pattern),
            (PatternKind::Regex, _) => pattern.to_owned(),
        };
        RegexBuilder::new(&source)
            .case_insensitive(!case_sensitive)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map(|r| Self(Inner::Regex(r)))
            .map_err(|e| match e {
                regex::Error::Syntax(s) => s,
                regex::Error::CompiledTooBig(limit) => {
                    format!("wyrażenie jest zbyt duże (limit {limit} B)")
                }
                other => other.to_string(),
            })
    }

    pub fn is_match(&self, hay: &[u8]) -> bool {
        match &self.0 {
            Inner::Text(f) => f.find(hay).is_some(),
            Inner::Regex(r) => r.is_match(hay),
        }
    }

    /// Byte ranges of non-overlapping, non-empty matches, in order.
    pub fn find_iter<'h>(&'h self, hay: &'h [u8]) -> Box<dyn Iterator<Item = Range<usize>> + 'h> {
        match &self.0 {
            Inner::Text(f) => {
                let len = f.needle().len();
                if len == 0 {
                    return Box::new(std::iter::empty());
                }
                Box::new(f.find_iter(hay).map(move |s| s..s + len))
            }
            Inner::Regex(r) => Box::new(
                r.find_iter(hay)
                    .filter(|m| !m.is_empty())
                    .map(|m| m.range()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(p: &Pattern, hay: &str) -> Vec<Range<usize>> {
        p.find_iter(hay.as_bytes()).collect()
    }

    #[test]
    fn text_case_sensitive_ranges() {
        let p = Pattern::new("ab", PatternKind::Text, true).unwrap();
        assert_eq!(ranges(&p, "ab xAB ab"), vec![0..2, 7..9]);
        assert!(!p.is_match(b"AB"));
    }

    #[test]
    fn text_case_insensitive_ranges() {
        let p = Pattern::new("ab", PatternKind::Text, false).unwrap();
        assert_eq!(ranges(&p, "ab xAB"), vec![0..2, 4..6]);
    }

    #[test]
    fn regex_skips_empty_matches() {
        let p = Pattern::new("x*", PatternKind::Regex, true).unwrap();
        assert_eq!(ranges(&p, "axxb"), vec![1..3]);
    }

    #[test]
    fn empty_text_matches_but_has_no_ranges() {
        let p = Pattern::new("", PatternKind::Text, true).unwrap();
        assert!(p.is_match(b"abc"));
        assert!(ranges(&p, "abc").is_empty());
    }

    #[test]
    fn unicode_case_folding() {
        let p = Pattern::new("ŻÓŁĆ", PatternKind::Text, false).unwrap();
        let hay = "x żółć";
        assert_eq!(ranges(&p, hay), vec![2..hay.len()]);
    }

    #[test]
    fn syntax_error_message() {
        let err = Pattern::new("(a", PatternKind::Regex, true).unwrap_err();
        assert!(err.contains("unclosed"), "{err}");
    }
}
