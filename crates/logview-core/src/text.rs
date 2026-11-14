//! Conversion of raw line bytes into displayable text.

use std::borrow::Cow;

/// A line prepared for display.
#[derive(Debug, PartialEq, Eq)]
pub struct DisplayLine<'a> {
    pub text: Cow<'a, str>,
    /// Bytes cut off because the line exceeded the display limit.
    pub hidden_bytes: usize,
}

/// Decodes `bytes` as UTF-8 (invalid sequences become U+FFFD), keeps at most
/// `max_bytes` bytes (cutting on a character boundary), expands tabs to
/// `tab_width` columns and replaces other control characters with U+FFFD.
pub fn display_line(bytes: &[u8], max_bytes: usize, tab_width: usize) -> DisplayLine<'_> {
    let cut = display_cut(bytes, max_bytes);
    DisplayLine {
        text: DisplayBuilder::new(tab_width).push(&bytes[..cut]),
        hidden_bytes: bytes.len() - cut,
    }
}

/// Converts consecutive segments of one line into display text, keeping the
/// column count across segments so tabs expand the same as for the whole
/// line. Used to render styled runs of a line separately.
#[derive(Debug)]
pub struct DisplayBuilder {
    tab_width: usize,
    column: usize,
}

impl DisplayBuilder {
    pub fn new(tab_width: usize) -> Self {
        Self {
            tab_width: tab_width.max(1),
            column: 0,
        }
    }

    /// Display text of the next segment of the line.
    pub fn push<'a>(&mut self, bytes: &'a [u8]) -> Cow<'a, str> {
        let text = String::from_utf8_lossy(bytes);
        if text.chars().any(|c| c.is_control()) {
            Cow::Owned(self.sanitize(&text))
        } else {
            self.column += text.chars().count();
            text
        }
    }

    fn sanitize(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 8);
        for c in text.chars() {
            match c {
                '\t' => {
                    let spaces = self.tab_width - self.column % self.tab_width;
                    out.extend(std::iter::repeat_n(' ', spaces));
                    self.column += spaces;
                }
                c if c.is_control() => {
                    out.push('\u{FFFD}');
                    self.column += 1;
                }
                c => {
                    out.push(c);
                    self.column += 1;
                }
            }
        }
        out
    }
}

/// Number of leading bytes of a line to display: at most `max_bytes`, cut on
/// a UTF-8 character boundary.
pub fn display_cut(bytes: &[u8], max_bytes: usize) -> usize {
    utf8_floor(bytes, max_bytes)
}

/// Largest cut position `<= max` that does not split a UTF-8 sequence.
fn utf8_floor(bytes: &[u8], max: usize) -> usize {
    if bytes.len() <= max {
        return bytes.len();
    }
    let mut cut = max;
    // Step back over at most 3 continuation bytes (10xxxxxx).
    while cut > 0 && max - cut < 3 && bytes[cut] & 0xC0 == 0x80 {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_keep_tab_columns() {
        let mut b = DisplayBuilder::new(4);
        let first = b.push(b"ab");
        let second = b.push(b"\tx");
        assert_eq!(format!("{first}{second}"), "ab  x");
        assert_eq!(display_line(b"ab\tx", 100, 4).text, "ab  x");
    }

    #[test]
    fn segments_with_multibyte_chars_count_columns() {
        let mut b = DisplayBuilder::new(4);
        b.push("żó".as_bytes());
        assert_eq!(b.push(b"\t"), "  ");
    }

    #[test]
    fn plain_ascii_is_borrowed() {
        let line = display_line(b"hello", 100, 4);
        assert!(matches!(line.text, Cow::Borrowed("hello")));
        assert_eq!(line.hidden_bytes, 0);
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        let line = display_line(b"a\xFFb", 100, 4);
        assert_eq!(line.text, "a\u{FFFD}b");
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let text = "ąćę"; // 2 bytes each
        for max in 0..=6 {
            let line = display_line(text.as_bytes(), max, 4);
            let kept = line.text.len();
            assert!(kept <= max);
            assert_eq!(kept % 2, 0, "max {max}");
            assert_eq!(kept + line.hidden_bytes, 6);
            assert!(!line.text.contains('\u{FFFD}'));
        }
    }

    #[test]
    fn tabs_expand_to_tab_stops() {
        assert_eq!(display_line(b"a\tb", 100, 4).text, "a   b");
        assert_eq!(display_line(b"\tx", 100, 4).text, "    x");
        assert_eq!(display_line(b"abcd\tx", 100, 4).text, "abcd    x");
    }

    #[test]
    fn control_chars_are_replaced() {
        assert_eq!(
            display_line(b"a\x00b\x1b", 100, 4).text,
            "a\u{FFFD}b\u{FFFD}"
        );
    }
}
