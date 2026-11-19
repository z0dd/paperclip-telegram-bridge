//! domain → Telegram: Markdown → HTML, splitting, topic names, texts (ru).

pub mod markdown;
pub mod text;

pub use text::{escape, split, truncate_chars, utf16_len, visible_len};

use url::Url;

/// Telegram limit for a topic name.
pub const TOPIC_NAME_MAX_CHARS: usize = 128;

#[derive(Debug, Clone)]
pub struct RenderConfig {
    pub max_chunks: usize,
    pub ui_url: Option<Url>,
}

/// One Telegram message: HTML and the same text as plain Markdown, the
/// fallback for `can't parse entities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub html: String,
    pub plain: String,
}

/// `«PET · PET Lead»`, at most 128 characters (ADR-0010 §2.6).
pub fn topic_name(issue_prefix: &str, agent_name: &str) -> String {
    let prefix = issue_prefix.trim();
    let agent = agent_name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = match (prefix.is_empty(), agent.is_empty()) {
        (false, false) => format!("{prefix} · {agent}"),
        (false, true) => prefix.to_string(),
        (true, false) => agent,
        (true, true) => "Paperclip".to_string(),
    };
    truncate_chars(&name, TOPIC_NAME_MAX_CHARS)
}

/// Link to the issue in the UI, when `PAPERCLIP_UI_URL` is set.
pub fn issue_url(identifier: &str, cfg: &RenderConfig) -> Option<Url> {
    let base = cfg.ui_url.as_ref()?;
    let prefix = identifier.split('-').next().unwrap_or(identifier);
    let mut url = base.clone();
    if let Ok(mut p) = url.path_segments_mut() {
        p.pop_if_empty().push(prefix).push("issues").push(identifier);
    }
    Some(url)
}

/// Splits Markdown into pieces of at most `limit` UTF-16 units; a code fence
/// cut by the split is closed at the end of a piece and reopened in the next.
pub fn split_markdown(md: &str, limit: usize) -> Vec<String> {
    // Room for the closing and reopening fence lines.
    let pieces = split(md, limit.saturating_sub(16).max(1));
    let mut out = Vec::with_capacity(pieces.len());
    let mut open: Option<String> = None;
    for p in pieces {
        let mut piece = match &open {
            Some(lang) => format!("```{lang}\n{p}"),
            None => p.clone(),
        };
        for line in p.lines() {
            if let Some(lang) = markdown::fence(line) {
                open = match open {
                    Some(_) => None,
                    None => Some(lang.to_string()),
                };
            }
        }
        if open.is_some() {
            piece.push_str("\n```");
        }
        out.push(piece);
    }
    out
}

/// Messages for one agent comment. Longer texts are cut to `max_chunks`
/// messages with a pointer to Paperclip.
pub fn comment_chunks(body: &str, identifier: Option<&str>, cfg: &RenderConfig) -> Vec<Chunk> {
    let body = body.trim();
    if body.is_empty() {
        return Vec::new();
    }
    let mut pieces = split_markdown(body, text::CHUNK_LIMIT);
    let cut = pieces.len() > cfg.max_chunks.max(1);
    pieces.truncate(cfg.max_chunks.max(1));
    let mut chunks: Vec<Chunk> = pieces
        .iter()
        .map(|p| Chunk {
            html: markdown::to_html(p),
            plain: p.clone(),
        })
        .collect();
    if cut && let Some(last) = chunks.last_mut() {
        let link = identifier.and_then(|i| issue_url(i, cfg));
        last.html.push_str("\n\n<i>… сообщение обрезано, полностью — в Paperclip");
        last.plain.push_str("\n\n… сообщение обрезано, полностью — в Paperclip");
        if let Some(url) = link {
            last.html
                .push_str(&format!(": <a href=\"{}\">{}</a>", escape(url.as_str()), escape(identifier.unwrap_or_default())));
            last.plain.push_str(&format!(": {url}"));
        }
        last.html.push_str("</i>");
    }
    chunks
}

/// Bridge messages in Russian.
pub mod notices {
    pub const WRITE_IN_TOPIC: &str =
        "ℹ️ Пишите в тему нужного агента: сообщения из General в Paperclip не передаются.";
    pub const TOPIC_NOT_LINKED: &str =
        "ℹ️ Эта тема не связана с беседой Paperclip, сообщение не передано.";
    pub const TEXT_ONLY: &str =
        "ℹ️ В Paperclip передаётся только текст: фото, файлы и стикеры не поддерживаются.";
    pub const PAUSED: &str =
        "⏸ Беседа на паузе, сообщение не передано. Возобновите беседу в Paperclip и отправьте снова.";
    pub const NOT_OWNER: &str = "❌ Paperclip не принял сообщение: ключ бриджа принадлежит не владельцу беседы. Проверьте настройку ключа.";
    pub const NOT_FOUND: &str =
        "❌ Беседа не найдена в Paperclip или Agent Chat выключен, сообщение не передано.";
    pub const QUEUED: &str =
        "⏳ Paperclip недоступен, сообщение в очереди и будет отправлено автоматически.";
    pub const KEY_REJECTED: &str = "🔑 Paperclip отклонил ключ бриджа (401). Сообщения не передаются, проверьте ключ. Повтор раз в 5 минут.";
    pub const AGENT_CHAT_DISABLED: &str =
        "⚠️ В Paperclip выключен Agent Chat: беседы недоступны. Бридж проверит снова позже.";

    pub fn rejected(message: &str) -> String {
        if message.is_empty() {
            "❌ Paperclip не принял сообщение.".into()
        } else {
            format!(
                "❌ Paperclip не принял сообщение: {}",
                super::escape(message)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: usize) -> RenderConfig {
        RenderConfig {
            max_chunks: max,
            ui_url: None,
        }
    }

    #[test]
    fn topic_names() {
        assert_eq!(topic_name("PET", "PET Lead"), "PET · PET Lead");
        assert_eq!(topic_name("", "Lead"), "Lead");
        assert_eq!(topic_name("NN", "  "), "NN");
        assert_eq!(topic_name("", ""), "Paperclip");
        let long = topic_name("PET", &"я".repeat(300));
        assert_eq!(long.chars().count(), TOPIC_NAME_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn chunks_fit_the_telegram_limit() {
        let body = "**word** <tag> & ".repeat(1500);
        let chunks = comment_chunks(&body, None, &cfg(10));
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(visible_len(&c.html) <= text::TELEGRAM_LIMIT, "{}", visible_len(&c.html));
            assert!(utf16_len(&c.plain) <= text::TELEGRAM_LIMIT);
        }
    }

    #[test]
    fn code_fence_is_reopened_across_pieces() {
        let body = format!("intro\n```py\n{}\n```\nouter", "x = 1\n".repeat(1200));
        let pieces = split_markdown(&body, text::CHUNK_LIMIT);
        assert!(pieces.len() > 1);
        for p in &pieces {
            let fences = p.lines().filter(|l| markdown::fence(l).is_some()).count();
            assert_eq!(fences % 2, 0, "{p}");
            assert!(utf16_len(p) <= text::CHUNK_LIMIT);
        }
        assert!(pieces[1].starts_with("```py\n"));
        let html: Vec<String> = pieces.iter().map(|p| markdown::to_html(p)).collect();
        assert!(html[0].contains("<pre><code class=\"language-py\">"));
        assert!(html.last().unwrap().ends_with("outer"));
    }

    #[test]
    fn long_comment_is_cut_with_a_pointer() {
        let mut c = cfg(2);
        c.ui_url = Some("https://paperclip.example".parse().unwrap());
        let body = "word ".repeat(3000);
        let chunks = comment_chunks(&body, Some("PET-4"), &c);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[1].html.contains("обрезано"));
        assert!(
            chunks[1]
                .html
                .contains("<a href=\"https://paperclip.example/PET/issues/PET-4\">PET-4</a>")
        );
        assert!(!chunks[0].html.contains("обрезано"));
    }

    #[test]
    fn empty_comment_has_no_chunks() {
        assert!(comment_chunks("  \n ", None, &cfg(3)).is_empty());
        assert_eq!(comment_chunks("ok", None, &cfg(3)).len(), 1);
    }
}
