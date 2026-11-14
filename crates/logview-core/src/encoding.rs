//! Text encodings and compression of log files.
//!
//! UTF-8 files are used as they are (memory-mapped). Files in other encodings
//! and compressed files are first converted into a temporary UTF-8 file (see
//! [`crate::prepare`]): UTF-16 cannot be indexed by `\n` bytes at all, and
//! for single-byte encodings conversion makes search and highlighting work
//! for non-ASCII letters (`ą` in Windows-1250 is not the UTF-8 `ą`).

use serde::{Deserialize, Serialize};

/// How to read the text of a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextEncoding {
    /// UTF-8, or UTF-16 if a BOM / the byte pattern says so.
    #[default]
    Auto,
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1250,
    Iso8859_2,
    Windows1252,
}

impl TextEncoding {
    /// Encodings offered to the user.
    pub const CHOICES: [TextEncoding; 7] = [
        TextEncoding::Auto,
        TextEncoding::Utf8,
        TextEncoding::Utf16Le,
        TextEncoding::Utf16Be,
        TextEncoding::Windows1250,
        TextEncoding::Iso8859_2,
        TextEncoding::Windows1252,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TextEncoding::Auto => "automatycznie",
            TextEncoding::Utf8 => "UTF-8",
            TextEncoding::Utf16Le => "UTF-16 LE",
            TextEncoding::Utf16Be => "UTF-16 BE",
            TextEncoding::Windows1250 => "Windows-1250 (środkowoeuropejskie)",
            TextEncoding::Iso8859_2 => "ISO-8859-2 (Latin-2)",
            TextEncoding::Windows1252 => "Windows-1252 (zachodnioeuropejskie)",
        }
    }

    /// Short name for the status bar.
    pub fn short(self) -> &'static str {
        match self {
            TextEncoding::Auto | TextEncoding::Utf8 => "UTF-8",
            TextEncoding::Utf16Le => "UTF-16 LE",
            TextEncoding::Utf16Be => "UTF-16 BE",
            TextEncoding::Windows1250 => "Windows-1250",
            TextEncoding::Iso8859_2 => "ISO-8859-2",
            TextEncoding::Windows1252 => "Windows-1252",
        }
    }

    /// The decoder, or `None` for UTF-8 (no conversion).
    pub fn decoder_encoding(self) -> Option<&'static encoding_rs::Encoding> {
        match self {
            TextEncoding::Auto | TextEncoding::Utf8 => None,
            TextEncoding::Utf16Le => Some(encoding_rs::UTF_16LE),
            TextEncoding::Utf16Be => Some(encoding_rs::UTF_16BE),
            TextEncoding::Windows1250 => Some(encoding_rs::WINDOWS_1250),
            TextEncoding::Iso8859_2 => Some(encoding_rs::ISO_8859_2),
            TextEncoding::Windows1252 => Some(encoding_rs::WINDOWS_1252),
        }
    }

    /// Resolves [`TextEncoding::Auto`] from the start of the text.
    pub fn resolve(self, head: &[u8]) -> TextEncoding {
        match self {
            TextEncoding::Auto => detect_utf16(head).unwrap_or(TextEncoding::Utf8),
            other => other,
        }
    }
}

/// UTF-16 from a byte order mark, or from the typical pattern of zero bytes
/// in every other position (ASCII text in UTF-16).
pub fn detect_utf16(head: &[u8]) -> Option<TextEncoding> {
    if head.starts_with(&[0xFF, 0xFE]) {
        return Some(TextEncoding::Utf16Le);
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return Some(TextEncoding::Utf16Be);
    }
    let sample = &head[..head.len().min(4096) & !1];
    if sample.len() < 16 {
        return None;
    }
    let pairs = sample.len() / 2;
    let zero_odd = sample
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|p| p[1] == 0 && p[0] != 0)
        .count();
    let zero_even = sample
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|p| p[0] == 0 && p[1] != 0)
        .count();
    if zero_odd * 10 >= pairs * 7 {
        Some(TextEncoding::Utf16Le)
    } else if zero_even * 10 >= pairs * 7 {
        Some(TextEncoding::Utf16Be)
    } else {
        None
    }
}

/// Whether `head` is valid UTF-8 (a sequence cut at the end is fine).
pub fn looks_like_utf8(head: &[u8]) -> bool {
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// Compression of a file, recognised by its first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Gzip,
    Zstd,
}

impl Codec {
    pub fn sniff(head: &[u8]) -> Option<Codec> {
        if head.starts_with(&[0x1F, 0x8B]) {
            Some(Codec::Gzip)
        } else if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            Some(Codec::Zstd)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::Gzip => "gzip",
            Codec::Zstd => "zstd",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn utf16be(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_be_bytes).collect()
    }

    #[test]
    fn detects_utf16_by_bom_and_pattern() {
        assert_eq!(
            detect_utf16(&[0xFF, 0xFE, b'a', 0]),
            Some(TextEncoding::Utf16Le)
        );
        assert_eq!(
            detect_utf16(&[0xFE, 0xFF, 0, b'a']),
            Some(TextEncoding::Utf16Be)
        );
        let text = "2026-01-01 12:00:00 INFO zażółć\n";
        assert_eq!(detect_utf16(&utf16le(text)), Some(TextEncoding::Utf16Le));
        assert_eq!(detect_utf16(&utf16be(text)), Some(TextEncoding::Utf16Be));
        assert_eq!(detect_utf16(text.as_bytes()), None);
        assert_eq!(detect_utf16(b"short"), None);
    }

    #[test]
    fn resolve_auto() {
        assert_eq!(TextEncoding::Auto.resolve(b"plain"), TextEncoding::Utf8);
        assert_eq!(
            TextEncoding::Auto.resolve(&[0xFF, 0xFE]),
            TextEncoding::Utf16Le
        );
        assert_eq!(
            TextEncoding::Windows1250.resolve(&[0xFF, 0xFE]),
            TextEncoding::Windows1250
        );
        assert!(TextEncoding::Utf8.decoder_encoding().is_none());
        assert!(TextEncoding::Windows1250.decoder_encoding().is_some());
    }

    #[test]
    fn utf8_check_allows_cut_sequence() {
        assert!(looks_like_utf8("zażółć".as_bytes()));
        assert!(looks_like_utf8(&"ż".as_bytes()[..1]), "cut at the end");
        assert!(!looks_like_utf8(b"za\xBF\xF3\xB3"), "Windows-1250 bytes");
    }

    #[test]
    fn sniffs_compression() {
        assert_eq!(Codec::sniff(&[0x1F, 0x8B, 8]), Some(Codec::Gzip));
        assert_eq!(Codec::sniff(&[0x28, 0xB5, 0x2F, 0xFD]), Some(Codec::Zstd));
        assert_eq!(Codec::sniff(b"text"), None);
    }

    #[test]
    fn serde_names() {
        #[derive(Serialize, Deserialize)]
        struct W {
            e: TextEncoding,
        }
        let text = toml::to_string(&W {
            e: TextEncoding::Windows1250,
        })
        .unwrap();
        assert_eq!(text.trim(), r#"e = "windows1250""#);
        let back: W = toml::from_str(r#"e = "utf16-le""#).unwrap();
        assert_eq!(back.e, TextEncoding::Utf16Le);
    }
}
