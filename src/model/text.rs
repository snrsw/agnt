//! Display sanitizing for text that came from outside agnt (provider titles,
//! previews, paths). Control characters are removed so a stored preview can
//! never act as terminal control, and width is measured in terminal cells.

use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// Remove ANSI escape sequences and the glyphs a TUI draws its frame with.
///
/// `sanitize` alone only drops the ESC byte, which leaves the parameters
/// behind as literal text (`[38;2;0;0;0m`). Provider output that is a screen
/// dump rather than prose — `claude logs` is one — needs the whole sequence
/// removed before any of it is readable.
///
/// Handles CSI (`ESC [ … final`), OSC (`ESC ] … BEL` or `ST`), and the
/// two-character escapes such as `ESC 7`. Box-drawing and block characters go
/// too, since a border carries nothing once it is out of its grid.
pub fn strip_terminal_sequences(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            if !is_frame_glyph(ch) {
                out.push(ch);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then a final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: runs to BEL or to ST (ESC \).
            Some(']') => {
                while let Some(c) = chars.next() {
                    match c {
                        '\x07' => break,
                        '\x1b' => {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                        _ => {}
                    }
                }
            }
            // Anything else is a two-character escape; both are dropped.
            _ => {}
        }
    }
    out
}

/// Box-drawing, block elements, and the geometric shapes a TUI uses as
/// borders and spinners.
fn is_frame_glyph(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259f}' | '\u{25a0}'..='\u{25ff}' | '\u{fffd}')
}

/// Drop control characters and collapse runs of whitespace into one space.
///
/// Whitespace is checked first so that `\t`, `\r`, and `\n` — control
/// characters that are also whitespace — become a separator rather than
/// vanishing. Any other control character is removed without leaving a gap.
pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

/// Cut `input` to at most `width` terminal cells, appending `…` when cut.
pub fn truncate_to_width(input: &str, width: usize) -> String {
    if input.width() <= width {
        return input.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let budget = width - 1; // room for the ellipsis
    let mut used = 0;
    let mut out = String::new();
    for ch in input.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

/// Truncate to `width` cells, then pad with spaces to exactly `width` cells.
pub fn pad_to_width(input: &str, width: usize) -> String {
    let mut s = truncate_to_width(input, width);
    let used = s.width();
    s.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    s
}

/// First `max_chars` characters of a sanitized excerpt, for titles derived
/// from the opening prompt.
pub fn excerpt(input: &str, max_chars: usize) -> String {
    let clean = sanitize(input);
    if clean.chars().count() <= max_chars {
        return clean;
    }
    let mut s: String = clean.chars().take(max_chars.saturating_sub(1)).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control_and_escape_sequences() {
        assert_eq!(sanitize("a\x1b[31mred\x1b[0m\tb\r\n c"), "a[31mred[0m b c");
        assert_eq!(sanitize("  leading"), "leading");
        assert_eq!(sanitize("trailing  "), "trailing");
    }

    #[test]
    fn truncates_by_cell_width_not_bytes() {
        // A session title can hold any text a provider returns, so the
        // wide-character case needs real double-width characters: these seven
        // are 21 bytes and occupy 14 cells, so a 6-cell budget fits two of
        // them plus the ellipsis.
        assert_eq!(truncate_to_width("認証処理の修正", 6), "認証…");
        assert_eq!(truncate_to_width("short", 10), "short");
        assert_eq!(truncate_to_width("abcdef", 4), "abc…");
        assert_eq!(truncate_to_width("abcdef", 0), "");
    }

    #[test]
    fn strips_whole_ansi_sequences_not_just_the_escape_byte() {
        // Shape taken from `claude logs`: truecolour SGR, cursor moves, save
        // and restore, and box-drawing borders.
        let raw =
            "\x1b[38;2;102;102;102mBrewed for 10s\x1b[39m\x1b[2B\x1b7\x1b8\u{2500}\u{2500}\u{252c} done";
        assert_eq!(sanitize(&strip_terminal_sequences(raw)), "Brewed for 10s done");
    }

    #[test]
    fn strips_osc_sequences_ended_by_bel_or_st() {
        assert_eq!(strip_terminal_sequences("a\x1b]0;window title\x07b"), "ab");
        assert_eq!(strip_terminal_sequences("a\x1b]8;;http://x\x1b\\b"), "ab");
    }

    #[test]
    fn plain_text_survives_stripping_unchanged() {
        assert_eq!(
            strip_terminal_sequences("Ran 1 shell command"),
            "Ran 1 shell command"
        );
        assert_eq!(strip_terminal_sequences("2026-04  48h 20m"), "2026-04  48h 20m");
        // Wide characters are not frame glyphs.
        assert_eq!(strip_terminal_sequences("認証"), "認証");
    }

    #[test]
    fn a_truncated_sequence_does_not_leak_parameters() {
        assert_eq!(strip_terminal_sequences("text\x1b[38;2;1"), "text");
        assert_eq!(strip_terminal_sequences("text\x1b"), "text");
    }

    #[test]
    fn excerpt_counts_chars() {
        assert_eq!(excerpt("fix the auth\nhandler now", 8), "fix the…");
        assert_eq!(excerpt("ok", 8), "ok");
    }
}
