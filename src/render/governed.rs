//! Governed cards: `toolAction` and `secretProposal` (architecture §8.3 j–k).
//! Own Russian templates; the server `prompt`, labels and `detailsMarkdown`
//! (full arguments JSON) are never shown. Always one message.

use time::OffsetDateTime;

use super::cards::{
    NoticeReason, RenderConfig, Rendered, Tail, cb, issue_ref_html, notice_text, status_lines,
};
use super::text::{CHUNK_LIMIT, escape, truncate_chars};
use super::{Op, button};
use crate::domain::*;
use crate::store::Delivery;
use crate::telegram::InlineKeyboard;
use crate::timeutil;

pub const PREVIEW_MAX: usize = 1500;
pub const ARGUMENTS_MAX: usize = 1500;
pub const JUSTIFICATION_MAX: usize = 1000;
pub const RESULT_SUMMARY_MAX: usize = 300;
/// Lifetime of the second confirmation step of a destructive action.
pub const CONFIRM_STEP_TTL: time::Duration = time::Duration::minutes(2);

pub fn is_expired(expires_at: Option<&str>, now: OffsetDateTime) -> bool {
    expires_at
        .and_then(timeutil::parse)
        .is_some_and(|t| t <= now)
}

/// The second step is active and not older than two minutes.
pub fn confirm_step_active(draft: Option<&Draft>, now: OffsetDateTime) -> bool {
    draft.is_some_and(|d| {
        d.awaiting == Some(Awaiting::GovernedConfirm)
            && timeutil::parse(&d.updated_at).is_some_and(|t| now - t < CONFIRM_STEP_TTL)
    })
}

fn deadline(expires_at: Option<&str>, now: OffsetDateTime) -> Option<String> {
    let t = timeutil::parse(expires_at?)?;
    Some(if t - now < time::Duration::hours(20) {
        timeutil::hhmm(t)
    } else {
        timeutil::ddmm_hhmm(t)
    })
}

fn risk_line(r: Risk) -> &'static str {
    match r {
        Risk::Read => "Риск: 👁 чтение",
        Risk::Write => "Риск: ✏️ запись",
        Risk::Destructive => "Риск: 🔴 НЕОБРАТИМО",
    }
}

fn governed_header(icon: &str, what: &str, d: &Delivery, cfg: &RenderConfig) -> String {
    let agent = d
        .snapshot
        .agent_name
        .as_deref()
        .unwrap_or(super::cards::DEFAULT_AGENT);
    let mut h = format!(
        "{icon} <b>Агент {} {}</b> · {}",
        escape(&truncate_chars(agent, 64)),
        what,
        issue_ref_html(&d.issue_ref, cfg)
    );
    if let Some(t) = d.snapshot.issue_title.as_deref().filter(|t| !t.is_empty()) {
        h.push_str(&format!("\n<i>{}</i>", escape(&truncate_chars(t, 200))));
    }
    h
}

/// Reject flow buttons shared by both kinds.
fn reject_keyboard(d: &Delivery, optional: bool, tail: &mut Tail, kb: &mut InlineKeyboard) {
    tail.line(if optional {
        "✍️ Причина отказа (необязательно): напишите её одним сообщением."
    } else {
        "✍️ Укажите причину отказа одним сообщением."
    });
    let mut row = Vec::new();
    if optional {
        row.push(button("Без причины", cb(d, Op::NoReason)));
    }
    row.push(button("Отмена", cb(d, Op::Cancel)));
    kb.row(row);
}

fn finish(header: String, body: String, tail: Tail, kb: InlineKeyboard) -> Rendered {
    let tail = tail.into_html();
    let mut html = format!("{header}\n\n{body}");
    if !tail.is_empty() {
        html.push_str("\n\n");
        html.push_str(&tail);
    }
    Rendered {
        messages: vec![html],
        keyboard: Some(kb).filter(|k| !k.is_empty()),
    }
}

pub fn tool_action(
    d: &Delivery,
    t: &ToolAction,
    draft: Option<&Draft>,
    cfg: &RenderConfig,
    now: OffsetDateTime,
    open: bool,
    notice: Option<NoticeReason>,
) -> Rendered {
    let header = governed_header("🛠", "просит выполнить действие", d, cfg);
    let mut service = format!("Действие: {}", truncate_chars(&t.tool_display_name, 200));
    if let Some(app) = t.app_display_name.as_deref() {
        service = format!("Сервис: {} · {service}", truncate_chars(app, 200));
    }
    let deadline_line = deadline(t.expires_at.as_deref(), now)
        .map(|s| format!("Действует до {s}"))
        .unwrap_or_default();

    // Budget: everything must fit one message. Arguments shrink first, then
    // the preview.
    let fixed =
        header.chars().count() + service.chars().count() + deadline_line.chars().count() + 400;
    let budget = CHUNK_LIMIT.saturating_sub(fixed);
    let marker = format!("…полностью — в Paperclip, {}", d.issue_ref);
    let preview_full = t.preview_markdown.trim();
    let mut args = if cfg.governed.show_arguments {
        t.arguments_summary_json
            .as_deref()
            .map(|a| truncate_chars(a.trim(), ARGUMENTS_MAX))
    } else {
        None
    };
    let preview_len = preview_full
        .chars()
        .count()
        .min(PREVIEW_MAX + marker.chars().count());
    if let Some(a) = &args {
        let room = budget.saturating_sub(preview_len);
        if a.chars().count() > room {
            args = (room > 40).then(|| truncate_chars(a, room));
        }
    }
    let args_len = args.as_ref().map_or(0, |a| a.chars().count());
    let preview_room = budget.saturating_sub(args_len).min(PREVIEW_MAX);
    let preview_cut = preview_full.chars().count() > preview_room;
    let preview = if preview_cut {
        format!(
            "{}\n{marker}",
            truncate_chars(
                preview_full,
                preview_room.saturating_sub(marker.chars().count() + 1)
            )
        )
    } else {
        preview_full.to_string()
    };

    let mut body = String::new();
    if preview_cut {
        body.push_str(&format!(
            "⚠️ Описание сокращено. Перед решением рекомендуется открыть {} в Paperclip\n",
            escape(&d.issue_ref)
        ));
    }
    body.push_str(&escape(&service));
    body.push('\n');
    body.push_str(risk_line(t.risk));
    if !preview.is_empty() {
        body.push_str("\nЧто произойдёт:\n");
        body.push_str(&escape(&preview));
    }
    if let Some(a) = &args {
        body.push_str(&format!("\nАргументы:\n<pre>{}</pre>", escape(a)));
    }
    if !deadline_line.is_empty() {
        body.push('\n');
        body.push_str(&escape(&deadline_line));
    }

    let mut tail = Tail::default();
    let mut kb = InlineKeyboard::default();
    if let Some(reason) = notice {
        tail.line(&notice_text(reason, &d.issue_ref));
    } else if open {
        let awaiting = draft.and_then(|x| x.awaiting);
        if is_expired(t.expires_at.as_deref(), now) {
            tail.line("⌛ Истекло, решения не принято");
        } else if confirm_step_active(draft, now) {
            tail.line_html("<b>🔴 Подтвердите необратимое действие</b>");
            let mut what = truncate_chars(&t.tool_display_name, 100);
            if let Some(app) = t.app_display_name.as_deref() {
                what.push_str(&format!(" · {}", truncate_chars(app, 100)));
            }
            tail.line(&format!("{what} · {}", d.issue_ref));
            kb.row(vec![
                button("🔴 Да, выполнить", cb(d, Op::GovernedConfirm)),
                button("Отмена", cb(d, Op::GovernedCancel)),
            ]);
        } else if let Some(Awaiting::RejectReason { optional }) = awaiting {
            reject_keyboard(d, optional, &mut tail, &mut kb);
        } else {
            kb.row(vec![
                button("✅ Разрешить", cb(d, Op::Accept)),
                button("❌ Отклонить", cb(d, Op::Reject)),
            ]);
        }
    }
    status_lines(d, &mut tail);
    finish(header, body, tail, kb)
}

pub fn secret_proposal(
    d: &Delivery,
    s: &SecretProposal,
    draft: Option<&Draft>,
    cfg: &RenderConfig,
    now: OffsetDateTime,
    open: bool,
    notice: Option<NoticeReason>,
) -> Rendered {
    let header = governed_header("🔑", "просит доступ к секрету", d, cfg);
    let mut lines = vec![
        format!(
            "Секрет: {} (существующий секрет компании)",
            truncate_chars(&s.source_secret_label, 200)
        ),
        format!(
            "Получит: агент {}, как {}",
            truncate_chars(s.target_agent_name.as_deref().unwrap_or("—"), 100),
            truncate_chars(&s.config_path, 200)
        ),
    ];
    if let Some(j) = s.justification.as_deref().filter(|j| !j.trim().is_empty()) {
        lines.push(format!(
            "Обоснование: {}",
            truncate_chars(j.trim(), JUSTIFICATION_MAX)
        ));
    }
    if let Some(dl) = deadline(s.expires_at.as_deref(), now) {
        lines.push(format!("Действует до {dl}"));
    }
    let body = escape(&lines.join("\n"));

    let mut tail = Tail::default();
    let mut kb = InlineKeyboard::default();
    if let Some(reason) = notice {
        tail.line(&notice_text(reason, &d.issue_ref));
    } else if open {
        if is_expired(s.expires_at.as_deref(), now) {
            tail.line("⌛ Истекло, решения не принято");
        } else if let Some(Awaiting::RejectReason { optional }) = draft.and_then(|x| x.awaiting) {
            reject_keyboard(d, optional, &mut tail, &mut kb);
        } else {
            kb.row(vec![
                button("✅ Привязать", cb(d, Op::Accept)),
                button("❌ Отклонить", cb(d, Op::Reject)),
            ]);
        }
    }
    status_lines(d, &mut tail);
    finish(header, body, tail, kb)
}
