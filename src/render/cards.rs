//! Card rendering: delivery state + draft → message texts and keyboard.
//! Pure functions; the static part of a card (header, prompt, details) never
//! depends on the draft, so the number of messages stays stable between edits.

use time::OffsetDateTime;
use url::Url;

use super::text::{
    CHUNK_LIMIT, TELEGRAM_LIMIT, escape, split, truncate_chars, truncate_utf16, visible_len,
};
use super::{Callback, Op, button};
use crate::config::GovernedConfig;
use crate::domain::*;
use crate::store::{Delivery, DeliveryStatus};
use crate::telegram::InlineKeyboard;

/// Upper bound of the state-dependent tail (visible characters).
pub const TAIL_MAX: usize = 3000;
/// Tail is appended to the last static piece only if that piece is short.
const APPEND_TAIL_IF_LAST_AT_MOST: usize = TELEGRAM_LIMIT - TAIL_MAX - 96;
/// Forms with more answerable questions cannot be answered via the API (API §6.1).
pub const MAX_FORM_QUESTIONS: usize = 20;
pub const DEFAULT_AGENT: &str = "агент";

#[derive(Debug, Clone)]
pub struct RenderConfig {
    pub max_chunks: usize,
    pub ui_url: Option<Url>,
    pub governed: GovernedConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub messages: Vec<String>,
    pub keyboard: Option<InlineKeyboard>,
}

/// Why a card is shown without buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeReason {
    Unsupported,
    FormTooLarge,
    GovernedDisabled,
    DestructiveUiOnly,
}

/// Whether the bridge can answer a card from Telegram.
pub fn notice_reason(i: &Interaction, cfg: &GovernedConfig) -> Option<NoticeReason> {
    use crate::config::DestructiveMode;
    match &i.body {
        InteractionBody::Unsupported { .. } => Some(NoticeReason::Unsupported),
        InteractionBody::Questions(f) if f.questions.len() > MAX_FORM_QUESTIONS => {
            Some(NoticeReason::FormTooLarge)
        }
        InteractionBody::ToolAction(_) | InteractionBody::SecretProposal(_) if !cfg.enabled => {
            Some(NoticeReason::GovernedDisabled)
        }
        InteractionBody::ToolAction(t)
            if t.risk == Risk::Destructive && cfg.destructive_mode == DestructiveMode::UiOnly =>
        {
            Some(NoticeReason::DestructiveUiOnly)
        }
        _ => None,
    }
}

/// Builder of the state-dependent tail with a size budget.
#[derive(Default)]
pub struct Tail {
    html: String,
    budget_hit: bool,
}

impl Tail {
    pub fn line_html(&mut self, html: &str) {
        if self.budget_hit {
            return;
        }
        let add = visible_len(html) + 1;
        if visible_len(&self.html) + add > TAIL_MAX {
            self.budget_hit = true;
            self.html.push_str("\n…");
            return;
        }
        if !self.html.is_empty() {
            self.html.push('\n');
        }
        self.html.push_str(html);
    }

    pub fn line(&mut self, raw: &str) {
        self.line_html(&escape(raw));
    }

    pub fn blank(&mut self) {
        if !self.html.is_empty() && !self.budget_hit {
            self.html.push('\n');
        }
    }

    pub fn into_html(self) -> String {
        self.html
    }
}

pub fn issue_ref_html(issue_ref: &str, cfg: &RenderConfig) -> String {
    match &cfg.ui_url {
        Some(base) => {
            let prefix = issue_ref.split('-').next().unwrap_or(issue_ref);
            let mut url = base.clone();
            if let Ok(mut p) = url.path_segments_mut() {
                p.pop_if_empty().push(prefix).push("issues").push(issue_ref);
            }
            format!(
                "<a href=\"{}\">{}</a>",
                escape(url.as_str()),
                escape(issue_ref)
            )
        }
        None => escape(issue_ref),
    }
}

/// `{icon} <b>{title} от {agent}</b> · PET-12` + italic issue title.
pub fn header(icon: &str, title: &str, d: &Delivery, cfg: &RenderConfig) -> String {
    let agent = d.snapshot.agent_name.as_deref().unwrap_or(DEFAULT_AGENT);
    let mut h = format!(
        "{icon} <b>{} {}</b> · {}",
        escape(title),
        escape(&truncate_chars(agent, 64)),
        issue_ref_html(&d.issue_ref, cfg)
    );
    if let Some(t) = d.snapshot.issue_title.as_deref().filter(|t| !t.is_empty()) {
        h.push_str(&format!("\n<i>{}</i>", escape(&truncate_chars(t, 200))));
    }
    h
}

/// Splits the static body, adds the header and the tail.
pub fn compose(
    header_html: &str,
    body_raw: &str,
    tail_html: &str,
    issue_ref: &str,
    max_chunks: usize,
) -> Vec<String> {
    let mut pieces = if body_raw.trim().is_empty() {
        vec![String::new()]
    } else {
        split(body_raw, CHUNK_LIMIT)
    };
    let max_chunks = max_chunks.max(1);
    let last_is_long = |p: &[String]| {
        p.last()
            .is_some_and(|l| visible_len(&escape(l)) > APPEND_TAIL_IF_LAST_AT_MOST)
    };
    // The tail must fit into the last allowed message, so a cut text also
    // leaves room for it.
    if pieces.len() > max_chunks
        || (pieces.len() == max_chunks && max_chunks > 1 && last_is_long(&pieces))
    {
        pieces.truncate(max_chunks);
        let marker = format!("\n…полный текст — в Paperclip, {issue_ref}");
        if let Some(last) = pieces.last_mut() {
            *last = format!(
                "{}{marker}",
                truncate_utf16(
                    last,
                    APPEND_TAIL_IF_LAST_AT_MOST - marker.chars().count() - 1
                )
            );
        }
    }
    let mut msgs: Vec<String> = pieces.iter().map(|p| escape(p)).collect();
    msgs[0] = if msgs[0].is_empty() {
        header_html.to_string()
    } else {
        format!("{header_html}\n\n{}", msgs[0])
    };
    // The placement depends only on the static part, so the number of
    // messages never changes between edits of the same card.
    let last = msgs.last_mut().expect("at least one message");
    if visible_len(last) <= APPEND_TAIL_IF_LAST_AT_MOST {
        if !tail_html.is_empty() {
            last.push_str("\n\n");
            last.push_str(tail_html);
        }
    } else if tail_html.is_empty() {
        msgs.push("Ответьте кнопкой ниже.".to_string());
    } else {
        msgs.push(tail_html.to_string());
    }
    msgs
}

fn join_raw(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Russian description of a resolved card status (architecture §8.3 h).
pub fn resolution_text(i: &Interaction) -> &'static str {
    let outcome = i.result.as_ref().and_then(|r| r.outcome.as_deref());
    match (&i.status, outcome) {
        (InteractionStatus::Accepted, _) => "принято",
        (InteractionStatus::Rejected, _) => "отклонено",
        (InteractionStatus::Answered, _) => "отвечено",
        (InteractionStatus::Cancelled, Some("withdrawn")) => "отозвано агентом",
        (InteractionStatus::Cancelled, _) => "отменено",
        (InteractionStatus::Expired, Some("issue_closed")) => "задача закрыта",
        (InteractionStatus::Expired, _) => "устарело",
        (InteractionStatus::Failed, _) => "ошибка",
        _ => "решено",
    }
}

pub fn resolved_elsewhere_line(i: &Interaction) -> String {
    format!("— Решено в Paperclip: {}", resolution_text(i))
}

/// Renders a delivery in its current state.
pub fn card(
    d: &Delivery,
    draft: Option<&Draft>,
    cfg: &RenderConfig,
    now: OffsetDateTime,
) -> Rendered {
    let i = &d.snapshot.interaction;
    let notice = notice_reason(i, &cfg.governed);
    let open = matches!(
        d.status,
        DeliveryStatus::New | DeliveryStatus::Sending | DeliveryStatus::Sent
    ) && notice.is_none();

    let (header_html, body_raw, mut tail, keyboard) = match &i.body {
        InteractionBody::ToolAction(t) => {
            return super::governed::tool_action(d, t, draft, cfg, now, open, notice);
        }
        InteractionBody::SecretProposal(s) => {
            return super::governed::secret_proposal(d, s, draft, cfg, now, open, notice);
        }
        InteractionBody::Confirmation(c) => {
            let (tail, kb) = if open {
                confirmation_tail(d, c, draft)
            } else {
                (Tail::default(), None)
            };
            (
                header("✅", "Подтверждение от", d, cfg),
                join_raw(&[Some(&c.prompt), c.details.as_deref()]),
                tail,
                kb,
            )
        }
        InteractionBody::Checkbox(c) => {
            let mut body = join_raw(&[Some(&c.prompt), c.details.as_deref()]);
            body.push_str("\n\n");
            body.push_str(&numbered(&c.options));
            let (tail, kb) = if open {
                checkbox_tail(d, c, draft)
            } else {
                (Tail::default(), None)
            };
            (header("☑️", "Выбор от", d, cfg), body, tail, kb)
        }
        InteractionBody::Questions(f) => {
            let title = if f.questions.len() > 1 {
                "Форма от"
            } else {
                "Вопрос от"
            };
            let body = join_raw(&[f.title.as_deref(), f.description.as_deref()]);
            let (tail, kb) = if open {
                super::forms::questions_tail(d, f, draft)
            } else {
                (Tail::default(), None)
            };
            let body = if body.is_empty() && f.questions.len() == 1 && !open {
                f.questions[0].prompt.clone()
            } else {
                body
            };
            (header("❓", title, d, cfg), body, tail, kb)
        }
        InteractionBody::Verdicts(v) => {
            let body = join_raw(&[Some(&v.prompt), v.details.as_deref()]);
            let (tail, kb) = if open {
                super::forms::verdicts_tail(d, v, draft)
            } else {
                (Tail::default(), None)
            };
            (header("📋", "Проверка от", d, cfg), body, tail, kb)
        }
        InteractionBody::Unsupported { .. } => (
            header("❓", "Запрос от", d, cfg),
            i.title.clone().unwrap_or_default(),
            Tail::default(),
            None,
        ),
    };

    if let Some(reason) = notice {
        tail.blank();
        tail.line(&notice_text(reason, &d.issue_ref));
    }
    status_lines(d, &mut tail);
    Rendered {
        messages: compose(
            &header_html,
            &body_raw,
            &tail.into_html(),
            &d.issue_ref,
            cfg.max_chunks,
        ),
        keyboard: keyboard.filter(|k| !k.is_empty()),
    }
}

pub fn notice_text(reason: NoticeReason, issue_ref: &str) -> String {
    match reason {
        NoticeReason::Unsupported => format!("Ответьте в Paperclip: {issue_ref}"),
        NoticeReason::FormTooLarge => {
            format!("Форма слишком большая для Telegram, ответьте в Paperclip: {issue_ref}")
        }
        NoticeReason::GovernedDisabled => format!("Решите в Paperclip: {issue_ref}"),
        NoticeReason::DestructiveUiOnly => {
            format!("Необратимое действие: решение только в Paperclip, {issue_ref}")
        }
    }
}

/// Lines describing the delivery state after the user acted.
pub fn status_lines(d: &Delivery, tail: &mut Tail) {
    match d.status {
        DeliveryStatus::Answering => {
            tail.blank();
            tail.line("⏳ Ответ отправляется…");
        }
        DeliveryStatus::Answered
        | DeliveryStatus::Closed
        | DeliveryStatus::Following
        | DeliveryStatus::Unsupported => {
            if let Some(line) = d.closed_reason.as_deref().filter(|l| !l.is_empty()) {
                tail.blank();
                tail.line(line);
            }
        }
        _ => {}
    }
}

pub fn numbered(options: &[Choice]) -> String {
    options
        .iter()
        .enumerate()
        .map(|(n, o)| match &o.description {
            Some(desc) => format!(
                "{}. {} — {}",
                n + 1,
                truncate_chars(&o.label, 200),
                truncate_chars(desc, 300)
            ),
            None => format!("{}. {}", n + 1, truncate_chars(&o.label, 200)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn cb(d: &Delivery, op: Op) -> Callback {
    Callback::new(op, d.id, d.generation)
}

fn confirmation_tail(
    d: &Delivery,
    c: &Confirmation,
    draft: Option<&Draft>,
) -> (Tail, Option<InlineKeyboard>) {
    let mut tail = Tail::default();
    let mut kb = InlineKeyboard::default();
    match draft.and_then(|dr| dr.awaiting) {
        Some(Awaiting::RejectReason { optional }) => {
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
        _ => kb.row(vec![
            button(
                c.accept_label.as_deref().unwrap_or("Approve"),
                cb(d, Op::Accept),
            ),
            button(
                c.reject_label.as_deref().unwrap_or("Reject"),
                cb(d, Op::Reject),
            ),
        ]),
    }
    (tail, Some(kb))
}

/// Current checkbox selection: the draft, or the card defaults.
pub fn checkbox_selection(c: &CheckboxConfirmation, draft: Option<&Draft>) -> Vec<String> {
    match draft {
        Some(dr) if dr.answers.selected_init => dr.answers.selected.clone(),
        _ => c
            .default_selected
            .iter()
            .filter(|id| c.options.iter().any(|o| &o.id == *id))
            .cloned()
            .collect(),
    }
}

pub fn checkbox_bounds_hint(c: &CheckboxConfirmation) -> Option<String> {
    match (c.min_selected, c.max_selected) {
        (Some(a), Some(b)) if a == b => Some(format!("Нужно выбрать: {a}")),
        (Some(a), Some(b)) => Some(format!("Нужно выбрать от {a} до {b}")),
        (Some(a), None) if a > 0 => Some(format!("Нужно выбрать не меньше {a}")),
        (None, Some(b)) => Some(format!("Можно выбрать не больше {b}")),
        _ => None,
    }
}

fn checkbox_tail(
    d: &Delivery,
    c: &CheckboxConfirmation,
    draft: Option<&Draft>,
) -> (Tail, Option<InlineKeyboard>) {
    let mut tail = Tail::default();
    let mut kb = InlineKeyboard::default();
    if let Some(Awaiting::RejectReason { optional }) = draft.and_then(|dr| dr.awaiting) {
        return confirmation_tail(
            d,
            &Confirmation {
                prompt: String::new(),
                details: None,
                accept_label: None,
                reject_label: None,
                reject_requires_reason: !optional,
                allow_decline_reason: optional,
            },
            draft,
        );
    }
    let selected = checkbox_selection(c, draft);
    if let Some(hint) = checkbox_bounds_hint(c) {
        tail.line(&hint);
    }
    for (n, o) in c.options.iter().enumerate() {
        let mark = if selected.contains(&o.id) {
            "☑"
        } else {
            "☐"
        };
        kb.row(vec![button(
            &format!("{mark} {}. {}", n + 1, o.label),
            cb(d, Op::Toggle).with_ab(0, n),
        )]);
    }
    kb.row(vec![
        button(
            &format!(
                "{} (выбрано {})",
                c.accept_label.as_deref().unwrap_or("Подтвердить"),
                selected.len()
            ),
            cb(d, Op::Accept),
        ),
        button(
            c.reject_label.as_deref().unwrap_or("Отклонить"),
            cb(d, Op::Reject),
        ),
    ]);
    (tail, Some(kb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DestructiveMode;
    use crate::store::Snapshot;

    pub fn cfg() -> RenderConfig {
        RenderConfig {
            max_chunks: 3,
            ui_url: None,
            governed: GovernedConfig {
                enabled: true,
                destructive_mode: DestructiveMode::Confirm,
                show_arguments: false,
                result_follow: std::time::Duration::from_secs(1800),
            },
        }
    }

    fn delivery(body: InteractionBody) -> Delivery {
        Delivery {
            id: 7,
            interaction_id: "card-1".into(),
            company_id: "c".into(),
            issue_id: "i".into(),
            issue_ref: "PET-12".into(),
            kind: "request_confirmation".into(),
            snapshot: Snapshot {
                interaction: Interaction {
                    id: "card-1".into(),
                    company_id: "c".into(),
                    issue_id: "i".into(),
                    kind: InteractionKind::RequestConfirmation,
                    status: InteractionStatus::Pending,
                    title: Some("Title".into()),
                    created_by_agent_id: None,
                    created_at: String::new(),
                    resolved_by_user_id: None,
                    resolved_by_agent_id: None,
                    addressee_agent_id: None,
                    addressee_user_id: None,
                    body,
                    result: None,
                },
                issue_title: Some("Issue <title>".into()),
                agent_name: Some("PET Lead".into()),
            },
            status: DeliveryStatus::Sent,
            generation: 2,
            closed_reason: None,
            follow_until: None,
            created_at: String::new(),
            updated_at: String::new(),
            messages: vec![(0, 100)],
        }
    }

    fn confirmation(prompt: &str, details: Option<&str>) -> InteractionBody {
        InteractionBody::Confirmation(Confirmation {
            prompt: prompt.into(),
            details: details.map(Into::into),
            accept_label: Some("Approve".into()),
            reject_label: Some("Reject".into()),
            reject_requires_reason: false,
            allow_decline_reason: false,
        })
    }

    #[test]
    fn confirmation_card_is_escaped_and_has_buttons() {
        let d = delivery(confirmation("Merge <b>now</b> & ship?", Some("details")));
        let r = card(&d, None, &cfg(), crate::timeutil::now());
        assert_eq!(r.messages.len(), 1);
        let m = &r.messages[0];
        assert!(
            m.starts_with(
                "✅ <b>Подтверждение от PET Lead</b> · PET-12\n<i>Issue &lt;title&gt;</i>"
            ),
            "{m}"
        );
        assert!(m.contains("Merge &lt;b&gt;now&lt;/b&gt; &amp; ship?"));
        let kb = r.keyboard.unwrap();
        assert_eq!(kb.inline_keyboard[0][0].text, "Approve");
        assert_eq!(kb.inline_keyboard[0][0].callback_data, "v1:ac:7:2");
        assert_eq!(kb.inline_keyboard[0][1].callback_data, "v1:rj:7:2");
    }

    #[test]
    fn long_details_are_split_and_capped() {
        let details = "word ".repeat(5000);
        let d = delivery(confirmation("Prompt", Some(&details)));
        let r = card(&d, None, &cfg(), crate::timeutil::now());
        assert_eq!(r.messages.len(), 3);
        for m in &r.messages {
            assert!(visible_len(m) <= TELEGRAM_LIMIT, "{}", visible_len(m));
        }
        assert!(r.messages[2].contains("…полный текст — в Paperclip, PET-12"));
        let mut c = cfg();
        c.max_chunks = 10;
        let r = card(&d, None, &c, crate::timeutil::now());
        assert!(r.messages.len() > 3);
    }

    #[test]
    fn message_count_is_stable_across_states() {
        let details = "x".repeat(3400);
        let mut d = delivery(confirmation("Prompt", Some(&details)));
        let open = card(&d, None, &cfg(), crate::timeutil::now());
        d.status = DeliveryStatus::Answered;
        d.closed_reason = Some("✅ Ответ отправлен: Approve".into());
        let done = card(&d, None, &cfg(), crate::timeutil::now());
        assert_eq!(open.messages.len(), done.messages.len());
        assert!(done.keyboard.is_none());
        assert!(done.messages.last().unwrap().contains("Ответ отправлен"));
    }

    #[test]
    fn reject_reason_prompt_buttons() {
        let d = delivery(confirmation("Ok?", None));
        let mut draft = Draft::new(7);
        draft.awaiting = Some(Awaiting::RejectReason { optional: true });
        let r = card(&d, Some(&draft), &cfg(), crate::timeutil::now());
        let kb = r.keyboard.unwrap();
        assert_eq!(kb.inline_keyboard[0][0].text, "Без причины");
        assert!(r.messages[0].contains("необязательно"));
    }

    #[test]
    fn checkbox_card_marks_selection() {
        let d = delivery(InteractionBody::Checkbox(CheckboxConfirmation {
            prompt: "Pick".into(),
            details: None,
            options: vec![
                Choice {
                    id: "a".into(),
                    label: "A".into(),
                    description: None,
                },
                Choice {
                    id: "b".into(),
                    label: "B".into(),
                    description: Some("bee".into()),
                },
            ],
            default_selected: vec!["a".into(), "zzz".into()],
            min_selected: Some(1),
            max_selected: Some(2),
            accept_label: None,
            reject_label: None,
            reject_requires_reason: false,
            allow_decline_reason: false,
        }));
        let r = card(&d, None, &cfg(), crate::timeutil::now());
        let kb = r.keyboard.unwrap().inline_keyboard;
        assert_eq!(kb[0][0].text, "☑ 1. A");
        assert_eq!(kb[1][0].text, "☐ 2. B");
        assert_eq!(kb[2][0].text, "Подтвердить (выбрано 1)");
        assert!(r.messages[0].contains("2. B — bee"));
        assert!(r.messages[0].contains("Нужно выбрать от 1 до 2"));
    }

    #[test]
    fn unsupported_card_has_no_buttons() {
        let d = delivery(InteractionBody::Unsupported {
            reason: UnsupportedReason::SuggestTasks,
        });
        let r = card(&d, None, &cfg(), crate::timeutil::now());
        assert!(r.keyboard.is_none());
        assert!(r.messages[0].contains("Ответьте в Paperclip: PET-12"));
    }

    #[test]
    fn ui_link_only_when_configured() {
        let d = delivery(confirmation("Ok?", None));
        let mut c = cfg();
        assert!(!card(&d, None, &c, crate::timeutil::now()).messages[0].contains("href"));
        c.ui_url = Some("https://paperclip.example".parse().unwrap());
        let m = &card(&d, None, &c, crate::timeutil::now()).messages[0];
        assert!(
            m.contains("<a href=\"https://paperclip.example/PET/issues/PET-12\">PET-12</a>"),
            "{m}"
        );
    }

    #[test]
    fn resolution_texts() {
        let mut d = delivery(confirmation("x", None));
        let i = &mut d.snapshot.interaction;
        i.status = InteractionStatus::Cancelled;
        i.result = Some(InteractionResult {
            outcome: Some("withdrawn".into()),
            ..Default::default()
        });
        assert_eq!(resolution_text(i), "отозвано агентом");
        i.status = InteractionStatus::Expired;
        i.result = Some(InteractionResult {
            outcome: Some("issue_closed".into()),
            ..Default::default()
        });
        assert_eq!(
            resolved_elsewhere_line(i),
            "— Решено в Paperclip: задача закрыта"
        );
    }
}
