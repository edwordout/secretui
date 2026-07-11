//! Rendering boundaries for text that originated outside SecretUI.
//!
//! Values returned by these helpers are for display only. Callers must retain and use the
//! original value for object matching and provider mutations.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use unicode_segmentation::UnicodeSegmentation;

pub const LABEL_GRAPHEME_LIMIT: usize = 256;
pub const PATH_GRAPHEME_LIMIT: usize = 512;
pub const ATTRIBUTE_KEY_GRAPHEME_LIMIT: usize = 256;
pub const ATTRIBUTE_VALUE_GRAPHEME_LIMIT: usize = 512;
pub const ERROR_GRAPHEME_LIMIT: usize = 1_024;
pub const DISPLAYED_ATTRIBUTE_LIMIT: usize = 256;

/// Escape terminal-active text and bound the result by rendered grapheme clusters.
///
/// Normal Unicode is preserved. Backslashes, C0/C1 controls, DEL, and Unicode bidi controls are
/// rendered as visible ASCII escapes. If the escaped result would exceed `grapheme_limit`, the
/// suffix records the original byte length and a short SHA-256 identifier so similarly prefixed
/// values remain distinguishable.
pub fn terminal_safe(text: &str, grapheme_limit: usize) -> String {
    let escaped = escape_terminal_text(text);
    if escaped.graphemes(true).count() <= grapheme_limit {
        return escaped;
    }

    let digest = Sha256::digest(text.as_bytes());
    let identifier = digest[..6]
        .iter()
        .fold(String::with_capacity(12), |mut identifier, byte| {
            write!(identifier, "{byte:02x}").expect("write to string");
            identifier
        });
    let suffix = format!("… [{} bytes; sha256={identifier}]", text.len());
    let suffix_len = suffix.graphemes(true).count();

    if suffix_len >= grapheme_limit {
        return suffix.graphemes(true).take(grapheme_limit).collect();
    }

    // Keep escape sequences intact by adding the escaped form of each source grapheme as a unit.
    let prefix_limit = grapheme_limit - suffix_len;
    let mut prefix = String::new();
    let mut prefix_len: usize = 0;
    for grapheme in text.graphemes(true) {
        let escaped_grapheme = escape_terminal_text(grapheme);
        let escaped_len = escaped_grapheme.graphemes(true).count();
        if prefix_len.saturating_add(escaped_len) > prefix_limit {
            break;
        }
        prefix.push_str(&escaped_grapheme);
        prefix_len += escaped_len;
    }
    prefix.push_str(&suffix);
    prefix
}

/// Render bytes that are expected to be text, replacing invalid UTF-8 before escaping it.
pub fn terminal_safe_bytes(bytes: &[u8], grapheme_limit: usize) -> String {
    terminal_safe(&String::from_utf8_lossy(bytes), grapheme_limit)
}

pub fn label(text: &str) -> String {
    terminal_safe(text, LABEL_GRAPHEME_LIMIT)
}

pub fn path(text: &str) -> String {
    terminal_safe(text, PATH_GRAPHEME_LIMIT)
}

pub fn attribute_key(text: &str) -> String {
    terminal_safe(text, ATTRIBUTE_KEY_GRAPHEME_LIMIT)
}

pub fn attribute_value(text: &str) -> String {
    terminal_safe(text, ATTRIBUTE_VALUE_GRAPHEME_LIMIT)
}

pub fn error(text: &str) -> String {
    terminal_safe(text, ERROR_GRAPHEME_LIMIT)
}

fn escape_terminal_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{1b}' => escaped.push_str("\\x1b"),
            '\0' => escaped.push_str("\\0"),
            character if character.is_control() || is_bidi_control(character) => {
                write!(escaped, "\\u{{{:x}}}", character as u32).expect("write to string");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_normal_unicode_and_escapes_terminal_active_text() {
        let text = "café שלום 👨‍👩‍👧\\\0\t\n\r\x1b\u{0085}\u{202e}";
        assert_eq!(
            terminal_safe(text, 512),
            "café שלום 👨‍👩‍👧\\\\\\0\\t\\n\\r\\x1b\\u{85}\\u{202e}"
        );
    }

    #[test]
    fn replacement_and_escapes_never_emit_raw_controls() {
        let rendered = terminal_safe_bytes(b"before\xff\x1b[31m\nafter", 128);
        assert!(rendered.contains('\u{fffd}'));
        assert!(rendered.contains("\\x1b[31m\\nafter"));
        assert!(!rendered.chars().any(char::is_control));
    }

    #[test]
    fn truncation_is_bounded_and_identifies_the_original() {
        let original = "e\u{301}".repeat(400);
        let rendered = terminal_safe(&original, LABEL_GRAPHEME_LIMIT);
        assert!(rendered.graphemes(true).count() <= LABEL_GRAPHEME_LIMIT);
        assert!(rendered.contains(&format!("[{} bytes; sha256=", original.len())));
        assert!(rendered.ends_with(']'));
        assert_eq!(rendered, terminal_safe(&original, LABEL_GRAPHEME_LIMIT));
    }

    #[test]
    fn truncation_does_not_split_visible_escape_sequences() {
        let original = "\u{202e}".repeat(100);
        let rendered = terminal_safe(&original, 80);
        let prefix = rendered.split('…').next().unwrap();
        assert!(prefix.is_empty() || prefix.ends_with('}'));
        assert!(rendered.graphemes(true).count() <= 80);
    }
}
