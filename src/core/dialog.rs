//! Telegram update → dialog state machine → one transaction (draft, outbox,
//! offset) → side effects after the commit (architecture §4.4, §8).

use super::allowlist::Admitted;
use super::{Ctx, Result, cards, commands, forms, governed};
use crate::domain::*;
use crate::paperclip::PaperclipApi;
use crate::render::{Callback, Op};
use crate::store::{CommitOutcome, Delivery, DeliveryStatus, StoreOp};
use crate::telegram::{CallbackQuery, Message, ReplyMarkup, TelegramApi, Update};
use crate::timeutil;

pub const STALE: &str = "Устарело";
pub const NOT_WAITING: &str =
    "Не жду текста. Ответьте кнопкой под вопросом или reply на него. /help";
pub const NO_TEXT_QUESTION: &str =
    "Сейчас нет вопросов, на которые можно ответить текстом. Сообщение не отправлено.";
pub const MAX_REASON_CHARS: usize = 4000;

/// Result of handling one update. `ops` are committed together with the offset.
#[derive(Debug, Default)]
pub struct Effects {
    pub ops: Vec<StoreOp>,
    /// `answerCallbackQuery` text.
    pub answer: Option<String>,
    /// Cards to re-render after the commit.
    pub refresh: Vec<i64>,
    /// Force-reply prompt for free text: (delivery, html).
    pub prompt: Option<(i64, String)>,
    /// Plain messages to send.
    pub messages: Vec<String>,
    pub enqueued: bool,
    /// Decision journal entry, logged after the commit with the update id.
    pub decision: Option<Decision>,
}

/// What the user decided, without any content (architecture §10, measure 4).
#[derive(Debug, Clone)]
pub struct Decision {
    pub delivery_id: i64,
    pub interaction_id: String,
    pub kind: String,
    pub risk: Option<&'static str>,
    pub action: &'static str,
}

fn risk_of(d: &Delivery) -> Option<&'static str> {
    match &d.snapshot.interaction.body {
        InteractionBody::ToolAction(t) => Some(match t.risk {
            Risk::Read => "read",
            Risk::Write => "write",
            Risk::Destructive => "destructive",
        }),
        InteractionBody::SecretProposal(_) => Some("secret_binding"),
        _ => None,
    }
}

impl Effects {
    pub fn answer(text: &str) -> Self {
        Self {
            answer: Some(text.to_string()),
            ..Default::default()
        }
    }

    pub fn message(html: impl Into<String>) -> Self {
        Self {
            messages: vec![html.into()],
            ..Default::default()
        }
    }

    pub fn stale(d: &Delivery) -> Self {
        let text = match d.status {
            DeliveryStatus::Answering => "Ответ уже отправляется",
            DeliveryStatus::Answered | DeliveryStatus::Following => "Ответ уже отправлен",
            DeliveryStatus::Closed => "Уже решено",
            _ => STALE,
        };
        Self {
            answer: Some(text.into()),
            refresh: vec![d.id],
            ..Default::default()
        }
    }

    /// Persists the draft, bumps the keyboard generation and re-renders.
    pub fn redraw(d: &Delivery, draft: Draft, answer: Option<&str>) -> Self {
        Self {
            ops: vec![StoreOp::PutDraft(draft), StoreOp::BumpGeneration(d.id)],
            answer: answer.map(str::to_string),
            refresh: vec![d.id],
            ..Default::default()
        }
    }

    /// Queues the answer for Paperclip.
    pub fn enqueue(d: &Delivery, resolution: Resolution, answer: &str) -> Self {
        let decision = Some(Decision {
            delivery_id: d.id,
            interaction_id: d.interaction_id.clone(),
            kind: d.kind.clone(),
            risk: risk_of(d),
            action: resolution.action_name(),
        });
        Self {
            decision,
            ops: vec![
                StoreOp::Enqueue {
                    delivery_id: d.id,
                    resolution,
                },
                StoreOp::BumpGeneration(d.id),
            ],
            answer: Some(answer.to_string()),
            refresh: vec![d.id],
            enqueued: true,
            ..Default::default()
        }
    }

    /// Starts waiting for text in `draft` (only one waiting at a time).
    pub fn await_text(d: &Delivery, mut draft: Draft, awaiting: Awaiting, prompt: String) -> Self {
        draft.awaiting = Some(awaiting);
        draft.updated_at = timeutil::now_string();
        Self {
            ops: vec![
                StoreOp::PutDraft(draft),
                StoreOp::ClearOtherTextAwaiting { keep: d.id },
                StoreOp::BumpGeneration(d.id),
            ],
            answer: None,
            refresh: vec![d.id],
            prompt: Some((d.id, prompt)),
            ..Default::default()
        }
    }
}

pub async fn handle_update<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    update: &Update,
) -> Result<()> {
    let (effects, callback_id) = match ctx.allowlist.check(update) {
        Err(reason) => {
            ctx.allowlist.log_rejection(update, reason);
            ctx.store.advance_offset(update.update_id).await?;
            return Ok(());
        }
        Ok(Admitted::Callback(cq)) => (on_callback(ctx, cq).await?, Some(cq.id.clone())),
        Ok(Admitted::Message(m)) => (on_message(ctx, m).await?, None),
    };
    let Effects {
        ops,
        answer,
        refresh,
        prompt,
        messages,
        enqueued,
        decision,
    } = effects;
    let outcome = ctx.store.commit_update(update.update_id, ops).await?;
    if outcome == CommitOutcome::Duplicate {
        tracing::debug!(update_id = update.update_id, "duplicate update skipped");
        return Ok(());
    }
    if let Some(dec) = decision {
        tracing::info!(
            update_id = update.update_id,
            delivery_id = dec.delivery_id,
            interaction_id = %dec.interaction_id,
            kind = %dec.kind,
            risk = dec.risk.unwrap_or("-"),
            action = dec.action,
            "decision from telegram queued"
        );
    }
    // Side effects after the commit. Failures here are not fatal: the state is
    // already consistent and the next render repairs the message.
    if let Some(id) = callback_id
        && let Err(e) = ctx.tg.answer(id, answer).await
    {
        tracing::debug!(class = e.class(), "callback answer failed");
    }
    if enqueued {
        ctx.outbox_notify.notify_one();
    }
    for did in refresh {
        if let Err(e) = cards::refresh(ctx, did).await {
            tracing::warn!(delivery_id = did, error = %e, "card refresh failed");
        }
    }
    for html in messages {
        cards::notify(ctx, html).await;
    }
    if let Some((did, html)) = prompt {
        match ctx.tg.send(html, Some(ReplyMarkup::ForceReply)).await {
            Ok(mid) => ctx.store.set_prompt_message(did, mid).await?,
            Err(e) => tracing::warn!(class = e.class(), "cannot send text prompt"),
        }
    }
    Ok(())
}

async fn on_callback<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    cq: &CallbackQuery,
) -> Result<Effects> {
    let Some(cb) = cq.data.as_deref().and_then(Callback::decode) else {
        return Ok(Effects::answer(STALE));
    };
    let Some(d) = ctx.store.delivery(cb.did).await? else {
        return Ok(Effects::answer(STALE));
    };
    if d.generation != cb.generation || d.status != DeliveryStatus::Sent {
        return Ok(Effects::stale(&d));
    }
    let draft = ctx.store.draft(d.id).await?;
    Ok(match &d.snapshot.interaction.body {
        InteractionBody::Confirmation(c) => confirmation_callback(&d, c, draft, cb),
        InteractionBody::Checkbox(c) => checkbox_callback(&d, c, draft, cb),
        InteractionBody::Questions(f) => forms::questions_callback(&d, f, draft, cb),
        InteractionBody::Verdicts(v) => forms::verdicts_callback(&d, v, draft, cb),
        InteractionBody::ToolAction(_) | InteractionBody::SecretProposal(_) => {
            governed::callback(ctx, &d, draft, cb).await
        }
        InteractionBody::Unsupported { .. } => Effects::stale(&d),
    })
}

/// Rejection with an optional or required reason (confirmation, checkbox).
pub fn reject_flow(
    d: &Delivery,
    draft: Option<Draft>,
    requires: bool,
    allows: bool,
    resolution: impl Fn(Option<String>) -> Resolution,
) -> Effects {
    if requires {
        Effects::await_text(
            d,
            draft.unwrap_or_else(|| Draft::new(d.id)),
            Awaiting::RejectReason { optional: false },
            "Укажите причину отказа одним сообщением. /cancel — отмена".into(),
        )
    } else if allows {
        Effects::await_text(
            d,
            draft.unwrap_or_else(|| Draft::new(d.id)),
            Awaiting::RejectReason { optional: true },
            "Причина отказа (необязательно): напишите её одним сообщением или нажмите «Без причины» под карточкой. /cancel — отмена".into(),
        )
    } else {
        Effects::enqueue(d, resolution(None), "Отправляю отказ…")
    }
}

/// Leaves a waiting state and shows the card again.
pub fn cancel_waiting(d: &Delivery, draft: Option<Draft>) -> Effects {
    match draft {
        Some(mut dr) => {
            dr.awaiting = None;
            dr.updated_at = timeutil::now_string();
            Effects::redraw(d, dr, Some("Отменено"))
        }
        None => Effects::answer("Отменено"),
    }
}

fn confirmation_callback(
    d: &Delivery,
    c: &Confirmation,
    draft: Option<Draft>,
    cb: Callback,
) -> Effects {
    let waiting_optional = matches!(
        draft.as_ref().and_then(|x| x.awaiting),
        Some(Awaiting::RejectReason { optional: true })
    );
    match cb.op {
        Op::Accept => Effects::enqueue(
            d,
            Resolution::Accept {
                selected_option_ids: None,
            },
            "Отправляю…",
        ),
        Op::Reject => reject_flow(
            d,
            draft,
            c.reject_requires_reason,
            c.allow_decline_reason,
            |reason| Resolution::Reject { reason },
        ),
        Op::NoReason if waiting_optional => {
            Effects::enqueue(d, Resolution::Reject { reason: None }, "Отправляю отказ…")
        }
        Op::Cancel => cancel_waiting(d, draft),
        _ => Effects::stale(d),
    }
}

fn checkbox_callback(
    d: &Delivery,
    c: &CheckboxConfirmation,
    draft: Option<Draft>,
    cb: Callback,
) -> Effects {
    let waiting_optional = matches!(
        draft.as_ref().and_then(|x| x.awaiting),
        Some(Awaiting::RejectReason { optional: true })
    );
    let selected = crate::render::cards::checkbox_selection(c, draft.as_ref());
    match cb.op {
        Op::Toggle => {
            let Some(opt) = cb.b.and_then(|b| c.options.get(b as usize)) else {
                return Effects::stale(d);
            };
            let mut sel = selected;
            if let Some(pos) = sel.iter().position(|x| *x == opt.id) {
                sel.remove(pos);
            } else {
                if c.max_selected.is_some_and(|m| sel.len() as u32 >= m) {
                    return Effects::answer(&format!(
                        "Можно выбрать не больше {}",
                        c.max_selected.unwrap_or_default()
                    ));
                }
                sel.push(opt.id.clone());
            }
            // Keep the card order of options.
            let ordered: Vec<String> = c
                .options
                .iter()
                .filter(|o| sel.contains(&o.id))
                .map(|o| o.id.clone())
                .collect();
            let mut dr = draft.unwrap_or_else(|| Draft::new(d.id));
            dr.answers.selected = ordered;
            dr.answers.selected_init = true;
            dr.updated_at = timeutil::now_string();
            Effects::redraw(d, dr, None)
        }
        Op::Accept => {
            let n = selected.len() as u32;
            if c.min_selected.is_some_and(|m| n < m) || c.max_selected.is_some_and(|m| n > m) {
                let hint = crate::render::cards::checkbox_bounds_hint(c)
                    .unwrap_or_else(|| "Проверьте выбор".into());
                return Effects::answer(&hint);
            }
            Effects::enqueue(
                d,
                Resolution::Accept {
                    selected_option_ids: Some(selected),
                },
                "Отправляю…",
            )
        }
        Op::Reject => reject_flow(
            d,
            draft,
            c.reject_requires_reason,
            c.allow_decline_reason,
            |reason| Resolution::Reject { reason },
        ),
        Op::NoReason if waiting_optional => {
            Effects::enqueue(d, Resolution::Reject { reason: None }, "Отправляю отказ…")
        }
        Op::Cancel => cancel_waiting(d, draft),
        _ => Effects::stale(d),
    }
}

async fn on_message<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    m: &Message,
) -> Result<Effects> {
    let Some(text) = m.text.as_deref() else {
        return Ok(Effects::message("Файлы и голос не поддерживаются."));
    };
    let trimmed = text.trim();
    if let Some(cmd) = trimmed.strip_prefix('/') {
        let name = cmd
            .split(|c: char| c.is_whitespace() || c == '@')
            .next()
            .unwrap_or_default();
        return match name {
            "cancel" => cancel_command(ctx).await,
            other => commands::handle(ctx, other).await,
        };
    }

    // Routing order of ADR-0009 §1: reply to a prompt, reply to a card,
    // a scenario waiting for text, then a chat-text card.
    if let Some(reply) = &m.reply_to_message {
        if let Some(dr) = ctx.store.draft_by_prompt_message(reply.message_id).await? {
            if let Some(d) = ctx.store.delivery(dr.delivery_id).await? {
                return Ok(text_for(ctx, &d, Some(dr), trimmed, false).await);
            }
        } else if let Some(d) = ctx.store.delivery_by_message(reply.message_id).await? {
            if forms::is_chat_text_question(&d)
                && let InteractionBody::Questions(f) = &d.snapshot.interaction.body
            {
                return Ok(forms::chat_text_answer(&d, f, trimmed));
            }
            let dr = ctx.store.draft(d.id).await?;
            return Ok(text_for(ctx, &d, dr, trimmed, true).await);
        }
        // A reply to anything else counts as a message without a reply.
    }
    if let Some(dr) = ctx.store.text_awaiting_draft().await?
        && let Some(d) = ctx.store.delivery(dr.delivery_id).await?
    {
        return Ok(text_for(ctx, &d, Some(dr), trimmed, false).await);
    }
    let open: Vec<Delivery> = ctx
        .store
        .deliveries_with_status(vec![DeliveryStatus::Sent])
        .await?
        .into_iter()
        .filter(forms::is_chat_text_question)
        .collect();
    Ok(match open.as_slice() {
        [] => Effects::message(NO_TEXT_QUESTION),
        [d] => match &d.snapshot.interaction.body {
            InteractionBody::Questions(f) => forms::chat_text_answer(d, f, trimmed),
            _ => Effects::message(NO_TEXT_QUESTION),
        },
        many => {
            let mut refs: Vec<String> = Vec::new();
            for d in many {
                let r = crate::render::escape(&d.issue_ref);
                if !refs.contains(&r) {
                    refs.push(r);
                }
            }
            Effects::message(format!(
                "Открыто несколько вопросов: {}. Ответьте reply на нужную карточку.",
                refs.join(", ")
            ))
        }
    })
}

/// Text for a known card: closed cards are refused, open ones go to `on_text`.
async fn text_for<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    draft: Option<Draft>,
    text: &str,
    via_reply: bool,
) -> Effects {
    if d.status != DeliveryStatus::Sent {
        return Effects::message(format!(
            "Карточка {} уже закрыта или ответ отправлен.",
            crate::render::escape(&d.issue_ref)
        ));
    }
    on_text(ctx, d, draft, text, via_reply).await
}

async fn on_text<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    draft: Option<Draft>,
    text: &str,
    via_reply: bool,
) -> Effects {
    let awaiting = draft.as_ref().and_then(|x| x.awaiting);
    match (&d.snapshot.interaction.body, awaiting) {
        (InteractionBody::ToolAction(_) | InteractionBody::SecretProposal(_), _) => {
            governed::text(ctx, d, draft, text).await
        }
        (
            InteractionBody::Confirmation(_) | InteractionBody::Checkbox(_),
            Some(Awaiting::RejectReason { .. }),
        ) => {
            if text.is_empty() {
                return Effects::message("Причина не может быть пустой. /cancel — отмена");
            }
            if text.chars().count() > MAX_REASON_CHARS {
                return Effects::message(format!(
                    "Причина слишком длинная (больше {MAX_REASON_CHARS} символов). Сократите её."
                ));
            }
            let mut e = Effects::enqueue(
                d,
                Resolution::Reject {
                    reason: Some(text.to_string()),
                },
                "",
            );
            e.answer = None;
            e
        }
        (InteractionBody::Questions(f), _) => forms::questions_text(d, f, draft, text, via_reply),
        (InteractionBody::Verdicts(v), Some(Awaiting::VerdictReason { .. })) => {
            forms::verdicts_text(d, v, draft, text)
        }
        _ => Effects::message(NOT_WAITING),
    }
}

async fn cancel_command<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) -> Result<Effects> {
    let Some(dr) = ctx.store.text_awaiting_draft().await? else {
        return Ok(Effects::message("Нечего отменять."));
    };
    let Some(d) = ctx.store.delivery(dr.delivery_id).await? else {
        return Ok(Effects::message("Нечего отменять."));
    };
    let mut e = cancel_waiting(&d, Some(dr));
    e.answer = None;
    e.messages.push("Ввод текста отменён.".into());
    Ok(e)
}
