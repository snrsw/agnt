//! Display sanitizing for text that came from outside agnt (provider titles,
//! previews, paths). Control characters are removed so a stored preview can
//! never act as terminal control, and width is measured in terminal cells.

use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

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
    fn excerpt_counts_chars() {
        assert_eq!(excerpt("fix the auth\nhandler now", 8), "fix the…");
        assert_eq!(excerpt("ok", 8), "ok");
    }
}
