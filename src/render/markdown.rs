//! Markdown of agent comments → Telegram HTML (ADR-0006, ADR-0010 §2.6).
//!
//! A deliberately small subset: fenced and inline code, bold, italic,
//! strikethrough, links, headings (bold), lists and quotes. Inline markup
//! never spans lines, so every tag is closed on the line that opened it and a
//! piece cut at a line boundary is always valid HTML. Anything unrecognised
//! stays literal text (escaped).

use super::text::escape;

/// Renders one piece of Markdown. Fences must be balanced within the piece
/// (see [`super::split_markdown`]); an unclosed fence runs to the end.
pub fn to_html(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut lines = md.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(lang) = fence(line) {
            let mut code = Vec::new();
            for l in lines.by_ref() {
                if fence(l).is_some() {
                    break;
                }
                code.push(l);
            }
            let class = if lang.is_empty() {
                String::new()
            } else {
                format!(" class=\"language-{}\"", escape(lang))
            };
            out.push(format!(
                "<pre><code{class}>{}</code></pre>",
                escape(&code.join("\n"))
            ));
            continue;
        }
        if quote(line).is_some() {
            let mut quoted = vec![inline(quote(line).unwrap_or_default())];
            while let Some(q) = lines.peek().and_then(|l| quote(l)) {
                quoted.push(inline(q));
                lines.next();
            }
            out.push(format!("<blockquote>{}</blockquote>", quoted.join("\n")));
            continue;
        }
        out.push(block_line(line));
    }
    out.join("\n")
}

/// ```` ```lang ```` → `Some("lang")`.
pub fn fence(line: &str) -> Option<&str> {
    let t = line.trim_start();
    t.strip_prefix("```").map(|rest| rest.trim())
}

fn quote(line: &str) -> Option<&str> {
    let t = line.trim_start();
    t.strip_prefix("> ").or_else(|| (t == ">").then_some(""))
}

fn block_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    // Headings: `# x` … `###### x` → bold.
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        return format!("<b>{}</b>", inline(trimmed[hashes..].trim()));
    }
    // Bullets: `- x`, `* x`, `+ x` → `• x`.
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return format!("{}• {}", escape(indent), inline(rest));
        }
    }
    // Horizontal rule.
    if trimmed.len() >= 3
        && (trimmed.bytes().all(|b| b == b'-') || trimmed.bytes().all(|b| b == b'*'))
    {
        return "──────────".into();
    }
    format!("{}{}", escape(indent), inline(trimmed))
}

/// Inline markup of one line.
pub fn inline(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Inline code: no markup inside.
        if c == '`'
            && let Some(end) = find(&chars, i + 1, "`")
            && end > i + 1
        {
            let code: String = chars[i + 1..end].iter().collect();
            out.push_str(&format!("<code>{}</code>", escape(&code)));
            i = end + 1;
            continue;
        }
        // Link: [text](http…)
        if c == '['
            && let Some((text, url, next)) = link(&chars, i)
        {
            out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                escape(&url),
                inline(&text)
            ));
            i = next;
            continue;
        }
        // Paired markers, longest first.
        let mut matched = false;
        for (marker, tag) in [
            ("**", "b"),
            ("__", "b"),
            ("~~", "s"),
            ("*", "i"),
            ("_", "i"),
        ] {
            if !starts_with(&chars, i, marker) {
                continue;
            }
            let len = marker.chars().count();
            // `_` and `__` only at word boundaries: snake_case stays as is.
            let underscore = marker.starts_with('_');
            if underscore && i > 0 && chars[i - 1].is_alphanumeric() {
                continue;
            }
            let Some(end) = find(&chars, i + len, marker) else {
                continue;
            };
            if end == i + len
                || chars[i + len].is_whitespace()
                || chars[end - 1].is_whitespace()
                || (underscore && chars.get(end + len).is_some_and(|c| c.is_alphanumeric()))
            {
                continue;
            }
            let inner: String = chars[i + len..end].iter().collect();
            out.push_str(&format!("<{tag}>{}</{tag}>", inline(&inner)));
            i = end + len;
            matched = true;
            break;
        }
        if matched {
            continue;
        }
        out.push_str(&escape(&c.to_string()));
        i += 1;
    }
    out
}

fn starts_with(chars: &[char], at: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    chars.len() >= at + p.len() && chars[at..at + p.len()] == p[..]
}

fn find(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    (from..chars.len()).find(|&j| starts_with(chars, j, pat))
}

fn link(chars: &[char], at: usize) -> Option<(String, String, usize)> {
    let close = find(chars, at + 1, "]")?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = find(chars, close + 2, ")")?;
    let text: String = chars[at + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    let url = url.trim().to_string();
    let scheme_ok = url.starts_with("https://") || url.starts_with("http://");
    if text.is_empty() || !scheme_ok || url.contains(char::is_whitespace) {
        return None;
    }
    Some((text, url, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markup() {
        assert_eq!(inline("**bold** and *it*"), "<b>bold</b> and <i>it</i>");
        assert_eq!(inline("__b__ _i_ ~~s~~"), "<b>b</b> <i>i</i> <s>s</s>");
        assert_eq!(inline("`a <b> *c*`"), "<code>a &lt;b&gt; *c*</code>");
        assert_eq!(
            inline("see [docs](https://x.example/a?b=1&c=2)"),
            "see <a href=\"https://x.example/a?b=1&amp;c=2\">docs</a>"
        );
        assert_eq!(
            inline("**bold with `code`**"),
            "<b>bold with <code>code</code></b>"
        );
    }

    #[test]
    fn unmatched_or_ambiguous_markers_stay_literal() {
        assert_eq!(inline("snake_case_name"), "snake_case_name");
        assert_eq!(inline("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(inline("**open"), "**open");
        assert_eq!(inline("a < b & c"), "a &lt; b &amp; c");
        assert_eq!(
            inline("[x](javascript:alert(1))"),
            "[x](javascript:alert(1))"
        );
        assert_eq!(inline("`"), "`");
    }

    #[test]
    fn blocks() {
        let md = "# Title\n\n- one\n* two\n> quoted\n> more\n\n```rust\nfn x() -> i32 { 1 < 2 }\n```\ntext";
        assert_eq!(
            to_html(md),
            "<b>Title</b>\n\n• one\n• two\n<blockquote>quoted\nmore</blockquote>\n\n\
             <pre><code class=\"language-rust\">fn x() -&gt; i32 { 1 &lt; 2 }</code></pre>\ntext"
        );
    }

    #[test]
    fn markup_inside_code_block_is_not_rendered() {
        assert_eq!(
            to_html("```\n**not bold** <tag>\n```"),
            "<pre><code>**not bold** &lt;tag&gt;</code></pre>"
        );
    }

    #[test]
    fn tags_are_balanced_on_every_line() {
        let md = "**a\nb** *c _d_ e* [l](https://e.x) `x";
        let html = to_html(md);
        for tag in ["b", "i", "a", "code", "pre", "s", "blockquote"] {
            assert_eq!(
                html.matches(&format!("<{tag}>")).count()
                    + html.matches(&format!("<{tag} ")).count(),
                html.matches(&format!("</{tag}>")).count(),
                "{tag}: {html}"
            );
        }
    }
}
