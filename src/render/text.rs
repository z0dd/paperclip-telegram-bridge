//! HTML escaping, UTF-16 lengths, splitting and truncation (architecture §8.1).

/// Raw text is split into pieces of at most this many UTF-16 units.
pub const CHUNK_LIMIT: usize = 3500;
/// Telegram limit for one message after entity parsing.
pub const TELEGRAM_LIMIT: usize = 4096;

/// Escapes `&`, `<`, `>` and `"`; everything else is plain text.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Length Telegram counts for an HTML message: tags removed, entities as one
/// character, in UTF-16 units.
pub fn visible_len(html: &str) -> usize {
    let mut n = 0;
    let mut in_tag = false;
    let mut in_entity = false;
    for c in html.chars() {
        match c {
            '<' if !in_entity => in_tag = true,
            '>' if in_tag => in_tag = false,
            '&' if !in_tag => {
                in_entity = true;
                n += 1;
            }
            ';' if in_entity => in_entity = false,
            _ if in_tag || in_entity => {}
            c => n += c.len_utf16(),
        }
    }
    n
}

/// Byte index of the longest prefix of `s` with at most `limit` UTF-16 units.
fn prefix_end(s: &str, limit: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > limit {
            return i;
        }
    }
    s.len()
}

/// Splits raw text into pieces of at most `limit` UTF-16 units, preferring
/// paragraph, then line, then word boundaries, then a hard cut. Surrogate
/// pairs are never split.
pub fn split(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut out = Vec::new();
    let mut rest = text.trim_matches('\n');
    while utf16_len(rest) > limit {
        let end = prefix_end(rest, limit);
        // Look two characters past the limit so that a separator right at the
        // boundary still counts; the piece itself (`..i`) never exceeds `end`.
        let probe_end = rest[end..]
            .char_indices()
            .nth(2)
            .map_or(rest.len(), |(i, _)| end + i);
        let window = &rest[..probe_end];
        let ok = |i: &usize| *i > 0 && *i <= end;
        let (cut, skip) = if let Some(i) = window.rfind("\n\n").filter(ok) {
            (i, 2)
        } else if let Some(i) = window.rfind('\n').filter(ok) {
            (i, 1)
        } else if let Some(i) = window.rfind(' ').filter(ok) {
            (i, 1)
        } else {
            (end.max(rest.chars().next().map_or(1, char::len_utf8)), 0)
        };
        out.push(rest[..cut].trim_end().to_string());
        rest = rest[cut + skip..].trim_start_matches(['\n', ' ']);
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// Truncates to at most `max` UTF-16 units including the trailing `…`.
pub fn truncate_utf16(s: &str, max: usize) -> String {
    if utf16_len(s) <= max {
        return s.to_string();
    }
    let end = prefix_end(s, max.saturating_sub(1));
    format!("{}…", s[..end].trim_end())
}

/// Truncates to at most `max` characters including the trailing `…`.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_special_chars() {
        assert_eq!(
            escape(r#"<b>a & "b"</b>"#),
            "&lt;b&gt;a &amp; &quot;b&quot;&lt;/b&gt;"
        );
        assert_eq!(escape("emoji 🚀 ok"), "emoji 🚀 ok");
    }

    #[test]
    fn visible_length_ignores_tags_and_counts_entities_once() {
        assert_eq!(visible_len("<b>ab</b> &amp; c"), 6);
        assert_eq!(visible_len(&escape("<<>>")), 4);
        assert_eq!(visible_len("🚀"), 2);
    }

    #[test]
    fn utf16_counts_surrogate_pairs() {
        assert_eq!(utf16_len("a"), 1);
        assert_eq!(utf16_len("я"), 1);
        assert_eq!(utf16_len("🚀"), 2);
    }

    #[test]
    fn split_prefers_paragraphs_then_lines_then_words() {
        let text = format!("{}\n\n{}", "a".repeat(8), "b".repeat(8));
        assert_eq!(split(&text, 12), vec!["a".repeat(8), "b".repeat(8)]);
        let text = format!("{}\n{}", "a".repeat(8), "b".repeat(8));
        assert_eq!(split(&text, 12), vec!["a".repeat(8), "b".repeat(8)]);
        assert_eq!(split("aaaa bbbb cccc", 9), vec!["aaaa bbbb", "cccc"]);
        assert_eq!(split("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(split("", 10), vec![""]);
    }

    #[test]
    fn split_never_breaks_surrogate_pairs() {
        let text = "🚀".repeat(10);
        let parts = split(&text, 5);
        for p in &parts {
            assert!(utf16_len(p) <= 5);
            assert!(p.chars().all(|c| c == '🚀'));
        }
        assert_eq!(parts.concat(), text);
    }

    #[test]
    fn split_pieces_fit_telegram_limit_after_escaping() {
        let text = "<&> ".repeat(3000);
        for piece in split(&text, CHUNK_LIMIT) {
            assert!(utf16_len(&piece) <= CHUNK_LIMIT);
            assert!(visible_len(&escape(&piece)) <= TELEGRAM_LIMIT);
        }
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(truncate_chars("abc", 4), "abc");
        assert_eq!(truncate_utf16("🚀🚀🚀", 4), "🚀…");
    }
}
