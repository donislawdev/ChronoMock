//! Characters that do not show as themselves.
//!
//! Text this tool did not write reaches a terminal and an evidence file: the names in a calendar or a
//! preset someone shared, the words a target sends about itself. A character that is invisible or
//! steers how the rest is shown can make that text say something it does not (R5-B1): a carriage
//! return and an erase-line sequence printed a fake `result:` line over the real one, and a
//! right-to-left override reverses what follows it on screen. Judged here, once, so the readers of
//! catalogue files and the quoting of a target's words cannot disagree about what counts.

/// The format characters (general category Cf) of Unicode 18.0, and the two separators, as inclusive
/// code point ranges. Control characters (Cc) are asked of the standard library, which knows them.
///
/// Source: `DerivedGeneralCategory-18.0.0.txt` of the Unicode Character Database
/// (unicode.org/Public/UCD/latest/ucd/extracted/), every line in category Cf. Written as numbers on
/// purpose - a tool that edits this file may turn an escaped character into the real one, which would
/// put the very characters this list is about into the source. A later Unicode version can add to the
/// category, and adding a range here is the whole change.
const HIDDEN_RANGES: [(u32, u32); 22] = [
    (0x00AD, 0x00AD),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x180E, 0x180E),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
    // Not Cf, but the line and paragraph separators (Zl, Zp): plenty of readers break a line on them,
    // including the JavaScript engine on the far side of the Chromium mode, so a "one-line" text with
    // one of them is two lines.
    (0x2028, 0x2029),
];

/// Whether `c` does not show as itself: a control character (Cc), a format character (Cf), or a line
/// or paragraph separator (Zl, Zp).
pub fn is_hidden_char(c: char) -> bool {
    let code = c as u32;
    c.is_control() || HIDDEN_RANGES.iter().any(|&(first, last)| (first..=last).contains(&code))
}

/// The first character of `text` that does not show as itself, with its position counted in
/// characters from 1, or `None` when every character shows.
pub fn first_hidden_char(text: &str) -> Option<(usize, char)> {
    text.chars().enumerate().find(|&(_, c)| is_hidden_char(c)).map(|(i, c)| (i + 1, c))
}

/// `text` with every character that does not show as itself written as its code, `\u{001b}` for an
/// escape, the way the quoting of a target's words writes it. For a message that quotes a text this
/// tool could not refuse before quoting it.
pub fn escape_hidden(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_hidden_char(c) {
            out.push_str(&format!("\\u{{{:04x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(code: u32) -> char {
        char::from_u32(code).unwrap()
    }

    /// The characters of the measured attack and of the classic reversed file name are caught, and
    /// so is every end of the table.
    #[test]
    fn controls_formats_and_separators_are_hidden() {
        for code in [0x1B, 0x0D, 0x0A, 0x00, 0x7F, 0x9B, 0x202E, 0x2066, 0x200B, 0x200D, 0xFEFF, 0x00AD, 0x2028, 0x2029, 0xE0001, 0xE007F] {
            assert!(is_hidden_char(ch(code)), "U+{code:04X}");
        }
        for (first, last) in HIDDEN_RANGES {
            assert!(is_hidden_char(ch(first)) && is_hidden_char(ch(last)), "U+{first:04X}..U+{last:04X}");
        }
    }

    /// Letters of the languages the catalogues are written in, and the punctuation they use, show.
    #[test]
    fn ordinary_text_shows() {
        for text in ["Boże Narodzenie", "Święto Niepodległości", "New Year's Day - observed", "ÄÖÜ ß éè ñ", "月末", "день"] {
            assert_eq!(first_hidden_char(text), None, "{text}");
        }
        // The neighbours of each range are outside it.
        for code in [0x00AC, 0x00AE, 0x200A, 0x2010, 0x2065, 0x2070, 0xFEFE, 0xFF00] {
            assert!(!is_hidden_char(ch(code)), "U+{code:04X}");
        }
    }

    /// The position is counted in characters, so a reader can find it in an editor.
    #[test]
    fn the_first_hidden_character_is_found_with_its_position() {
        let text: String = ['ś', 'w', ch(0x202E), 'x'].iter().collect();
        assert_eq!(first_hidden_char(&text), Some((3, ch(0x202E))));
        assert_eq!(first_hidden_char(""), None);
    }

    /// Every hidden character comes out as its code, and everything else as itself.
    #[test]
    fn hidden_characters_are_escaped_and_the_rest_is_kept() {
        let text: String = ['ś', ch(0x1B), '[', '2', 'K', ch(0x202E), 'x'].iter().collect();
        let escaped = escape_hidden(&text);
        assert_eq!(escaped, "ś\\u{001b}[2K\\u{202e}x");
        assert_eq!(first_hidden_char(&escaped), None);
        assert_eq!(escape_hidden("Boże Narodzenie"), "Boże Narodzenie");
    }
}
