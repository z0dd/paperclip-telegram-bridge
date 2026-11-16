//! Governed confirmations: `toolAction` and `secretProposal` (API §6.6,
//! architecture §8.3 j–k). Outcomes are decided by the re-read card state,
//! not by HTTP codes: the server answers repeats with `200` and terminal
//! tool-gateway states with `409` without a code or `500`.

use super::dialog::{Effects, cancel_waiting};
use super::outbox::{resolved_by_us, retry_later};
use super::{Ctx, Result, cards};
use crate::config::DestructiveMode;
use crate::domain::*;
use crate::paperclip::{PaperclipApi, PaperclipError};
use crate::render::cards::resolved_elsewhere_line;
use crate::render::governed::{RESULT_SUMMARY_MAX, confirm_step_active, is_expired};
use crate::render::{Callback, Op, escape, truncate_chars};
use crate::retry::Backoff;
use crate::store::{Delivery, DeliveryStatus, OutboxEntry, StoreOp};
use crate::telegram::TelegramApi;
use crate::timeutil;

pub const APPROVE_NOTE: &str = "Approved via Telegram bridge";
pub const REJECT_NOTE: &str = "Rejected via Telegram bridge";
/// Repeats when the card is still pending for an unclear reason.
pub const MAX_UNCLEAR: u32 = 3;
const UNCLEAR_PREFIX: &str = "governed_unclear:";

fn expires_at(d: &Delivery) -> Option<&str> {
    match &d.snapshot.interaction.body {
        InteractionBody::ToolAction(t) => t.expires_at.as_deref(),
        InteractionBody::SecretProposal(s) => s.expires_at.as_deref(),
        _ => None,
    }
}

/// Re-reads the card before queuing a decision. `Err` = do not queue, with
/// the line to show. Paperclip being unreachable does not block: the outbox
/// checks again right before sending.
async fn precheck<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
) -> std::result::Result<(), String> {
    let Ok(list) = ctx.paperclip.interactions(&d.issue_id).await else {
        return Ok(());
    };
    match list.iter().find(|i| i.id == d.interaction_id) {
        None => Err("— Действие изменилось или устарело".into()),
        Some(i) if !i.status.is_pending() => Err(resolved_elsewhere_line(i)),
        Some(i) if changed(&d.snapshot.interaction, i) => {
            Err("— Действие изменилось или устарело".into())
        }
        Some(_) => Ok(()),
    }
}

/// Arguments hash or target revision differ from what the user saw.
fn changed(seen: &Interaction, now: &Interaction) -> bool {
    match (&seen.body, &now.body) {
        (InteractionBody::ToolAction(a), InteractionBody::ToolAction(b)) => {
            a.arguments_hash != b.arguments_hash || a.target_revision_id != b.target_revision_id
        }
        (InteractionBody::SecretProposal(_), InteractionBody::SecretProposal(_)) => false,
        _ => true,
    }
}

fn closed_effects(d: &Delivery, line: String, answer: &str) -> Effects {
    Effects {
        ops: vec![
            StoreOp::SetStatus {
                delivery_id: d.id,
                status: DeliveryStatus::Closed,
                closed_reason: Some(line),
            },
            StoreOp::DeleteDraft(d.id),
            StoreOp::BumpGeneration(d.id),
        ],
        answer: Some(answer.into()),
        refresh: vec![d.id],
        ..Default::default()
    }
}

pub async fn callback<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    draft: Option<Draft>,
    cb: Callback,
) -> Effects {
    let now = timeutil::now();
    let (destructive, hash, revision, reason_required) = match &d.snapshot.interaction.body {
        InteractionBody::ToolAction(t) => (
            t.risk == Risk::Destructive,
            Some(t.arguments_hash.clone()),
            t.target_revision_id.clone(),
            false,
        ),
        InteractionBody::SecretProposal(_) => (false, None, None, true),
        _ => return Effects::stale(d),
    };
    if destructive && ctx.cfg.governed.destructive_mode == DestructiveMode::UiOnly {
        return Effects::stale(d);
    }
    let accept = Resolution::GovernedAccept {
        destructive,
        arguments_hash: hash,
        target_revision_id: revision,
    };
    let waiting_optional = matches!(
        draft.as_ref().and_then(|x| x.awaiting),
        Some(Awaiting::RejectReason { optional: true })
    );
    match cb.op {
        Op::Accept | Op::GovernedConfirm => {
            if is_expired(expires_at(d), now) {
                let mut e = Effects::answer("Срок истёк");
                e.refresh.push(d.id);
                return e;
            }
            if cb.op == Op::Accept && destructive {
                // First press opens the second step in the same message.
                let mut dr = draft.unwrap_or_else(|| Draft::new(d.id));
                dr.awaiting = Some(Awaiting::GovernedConfirm);
                dr.updated_at = timeutil::now_string();
                return Effects::redraw(d, dr, Some("Подтвердите необратимое действие"));
            }
            if cb.op == Op::GovernedConfirm && !confirm_step_active(draft.as_ref(), now) {
                let mut dr = draft.unwrap_or_else(|| Draft::new(d.id));
                dr.awaiting = None;
                return Effects::redraw(d, dr, Some("Время подтверждения истекло"));
            }
            if let Err(line) = precheck(ctx, d).await {
                return closed_effects(d, line, "Действие изменилось или устарело");
            }
            Effects::enqueue(d, accept, "Разрешено, отправляю…")
        }
        Op::GovernedCancel => {
            let mut dr = draft.unwrap_or_else(|| Draft::new(d.id));
            dr.awaiting = None;
            Effects::redraw(d, dr, Some("Отменено"))
        }
        Op::Reject => {
            let (awaiting, prompt) = if reason_required {
                (
                    Awaiting::RejectReason { optional: false },
                    "Укажите причину отказа одним сообщением. /cancel — отмена",
                )
            } else {
                (
                    Awaiting::RejectReason { optional: true },
                    "Причина отказа (необязательно): напишите её одним сообщением или нажмите «Без причины» под карточкой. /cancel — отмена",
                )
            };
            Effects::await_text(
                d,
                draft.unwrap_or_else(|| Draft::new(d.id)),
                awaiting,
                prompt.into(),
            )
        }
        Op::NoReason if waiting_optional => Effects::enqueue(
            d,
            Resolution::GovernedReject {
                reason: None,
                destructive,
            },
            "Отправляю отказ…",
        ),
        Op::Cancel => cancel_waiting(d, draft),
        _ => Effects::stale(d),
    }
}

pub async fn text<P: PaperclipApi, T: TelegramApi>(
    _ctx: &Ctx<P, T>,
    d: &Delivery,
    draft: Option<Draft>,
    text: &str,
) -> Effects {
    let Some(Awaiting::RejectReason { .. }) = draft.as_ref().and_then(|x| x.awaiting) else {
        return Effects::message(super::dialog::NOT_WAITING);
    };
    if text.is_empty() {
        return Effects::message("Причина не может быть пустой. /cancel — отмена");
    }
    if text.chars().count() > super::dialog::MAX_REASON_CHARS {
        return Effects::message(format!(
            "Причина слишком длинная (больше {} символов). Сократите её.",
            super::dialog::MAX_REASON_CHARS
        ));
    }
    let destructive = matches!(&d.snapshot.interaction.body, InteractionBody::ToolAction(t) if t.risk == Risk::Destructive);
    let mut e = Effects::enqueue(
        d,
        Resolution::GovernedReject {
            reason: Some(text.to_string()),
            destructive,
        },
        "",
    );
    e.answer = None;
    e
}

fn unclear_count(e: &OutboxEntry) -> u32 {
    e.last_error
        .as_deref()
        .and_then(|s| s.strip_prefix(UNCLEAR_PREFIX))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Sends a governed decision (outbox worker).
pub async fn process<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    r: &Resolution,
) -> Result<()> {
    let current = match read_card(ctx, d).await {
        Ok(Some(i)) => i,
        Ok(None) => return close(ctx, d, e, "— Карточка не найдена в Paperclip".into()).await,
        Err(err) => return retry_later(ctx, d, e, &err).await,
    };
    if !current.status.is_pending() {
        return settle(ctx, d, e, r, &current).await;
    }
    match r {
        Resolution::GovernedAccept {
            destructive,
            arguments_hash,
            target_revision_id,
        } => {
            if is_expired(expires_at(d), timeutil::now()) {
                return close(ctx, d, e, "⌛ Истекло, решения не принято".into()).await;
            }
            if let InteractionBody::ToolAction(t) = &current.body
                && (Some(&t.arguments_hash) != arguments_hash.as_ref()
                    || t.target_revision_id != *target_revision_id)
            {
                return close(
                    ctx,
                    d,
                    e,
                    "— Действие изменилось или устарело, решение не отправлено".into(),
                )
                .await;
            }
            if *destructive && let InteractionBody::ToolAction(t) = &current.body {
                // Step 1: the formal approval created by the tool gateway.
                let approvals = match ctx.paperclip.issue_approvals(&d.issue_id).await {
                    Ok(a) => a,
                    Err(err) => return retry_later(ctx, d, e, &err).await,
                };
                let Some(approval) = approvals.into_iter().find(|a| a.matches_action(t)) else {
                    let line = format!(
                        "— Не удалось одобрить из Telegram, решите в Paperclip: {}",
                        d.issue_ref
                    );
                    return close(ctx, d, e, line).await;
                };
                match approval.status.as_str() {
                    "approved" => {}
                    "pending" => {
                        if let Err(err) = ctx
                            .paperclip
                            .approve_approval(&approval.id, APPROVE_NOTE)
                            .await
                        {
                            tracing::warn!(delivery_id = d.id, class = %err.class(), "formal approval failed");
                            return unclear(ctx, d, e, r, &err).await;
                        }
                    }
                    _ => {
                        return close(ctx, d, e, "— Решено в Paperclip: отклонено".into()).await;
                    }
                }
            }
            // Step 2: accept the card (never with rememberAction).
            match ctx
                .paperclip
                .governed_accept(&d.issue_id, &d.interaction_id)
                .await
            {
                Ok(Some(rec)) if !rec.status.is_pending() => settle(ctx, d, e, r, &rec).await,
                Ok(_) => match read_card(ctx, d).await {
                    Ok(Some(i)) if !i.status.is_pending() => settle(ctx, d, e, r, &i).await,
                    _ => unclear(ctx, d, e, r, &PaperclipError::Decode).await,
                },
                Err(err) => unclear(ctx, d, e, r, &err).await,
            }
        }
        Resolution::GovernedReject {
            reason,
            destructive: _,
        } => match ctx
            .paperclip
            .governed_reject(&d.issue_id, &d.interaction_id, reason.as_deref())
            .await
        {
            Ok(Some(rec)) if !rec.status.is_pending() => settle(ctx, d, e, r, &rec).await,
            Ok(_) => match read_card(ctx, d).await {
                Ok(Some(i)) if !i.status.is_pending() => settle(ctx, d, e, r, &i).await,
                _ => unclear(ctx, d, e, r, &PaperclipError::Decode).await,
            },
            Err(err) => unclear(ctx, d, e, r, &err).await,
        },
        _ => close(ctx, d, e, "— Ответ не отправлен".into()).await,
    }
}

async fn read_card<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
) -> std::result::Result<Option<Interaction>, PaperclipError> {
    Ok(ctx
        .paperclip
        .interactions(&d.issue_id)
        .await?
        .into_iter()
        .find(|i| i.id == d.interaction_id))
}

/// Any non-2xx: decide by the re-read state (API §6.6, rule 2).
async fn unclear<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    r: &Resolution,
    err: &PaperclipError,
) -> Result<()> {
    let current = match read_card(ctx, d).await {
        Ok(Some(i)) => i,
        Ok(None) => return close(ctx, d, e, "— Карточка не найдена в Paperclip".into()).await,
        // Cannot read the state (network): keep the decision and retry.
        Err(read_err) => return retry_later(ctx, d, e, &read_err).await,
    };
    if !current.status.is_pending() {
        return settle(ctx, d, e, r, &current).await;
    }
    let n = unclear_count(e) + 1;
    tracing::warn!(
        delivery_id = d.id,
        class = %err.class(),
        unclear = n,
        "governed decision not applied yet"
    );
    if n >= MAX_UNCLEAR {
        return close(
            ctx,
            d,
            e,
            format!("— Не удалось, ответьте в Paperclip: {}", d.issue_ref),
        )
        .await;
    }
    let delay = Backoff::from_config(&ctx.cfg.retry).delay(e.attempts + 1);
    let next = timeutil::format(timeutil::add(timeutil::now(), delay));
    ctx.store
        .outbox_retry(e.id, e.attempts + 1, next, format!("{UNCLEAR_PREFIX}{n}"))
        .await?;
    Ok(())
}

/// The card is resolved: ours or somebody else's.
async fn settle<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    r: &Resolution,
    current: &Interaction,
) -> Result<()> {
    let ours = super::outbox::result_matches(current, r)
        && resolved_by_us(current, ctx.self_user_id().as_deref());
    if let Some(uid) = current.resolved_by_user_id.as_deref().filter(|_| ours) {
        ctx.learn_self_user_id(uid).await;
    }
    if !ours {
        tracing::info!(
            delivery_id = d.id,
            interaction_id = %d.interaction_id,
            status = current.status.as_str(),
            outcome = "resolved_elsewhere",
            "governed decision not sent"
        );
        return close(ctx, d, e, resolved_elsewhere_line(current)).await;
    }
    super::sync::paperclip_ok(ctx).await;
    if let Resolution::GovernedReject {
        destructive: true, ..
    } = r
    {
        reject_linked_approval(ctx, d).await;
    }
    let (status, line) = outcome_line(&d.snapshot.interaction, current, r);
    tracing::info!(
        delivery_id = d.id,
        interaction_id = %d.interaction_id,
        kind = %d.kind,
        action = r.action_name(),
        outcome = status.as_str(),
        "governed decision applied"
    );
    ctx.store
        .outbox_done(e.id, d.id, status, Some(line))
        .await?;
    if status == DeliveryStatus::Following {
        let until = timeutil::add(timeutil::now(), ctx.cfg.governed.result_follow);
        ctx.store
            .set_following(d.id, timeutil::format(until))
            .await?;
    }
    cards::refresh(ctx, d.id).await
}

/// After a rejected destructive action the server leaves the approval open.
async fn reject_linked_approval<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>, d: &Delivery) {
    let InteractionBody::ToolAction(t) = &d.snapshot.interaction.body else {
        return;
    };
    match ctx.paperclip.issue_approvals(&d.issue_id).await {
        Ok(list) => {
            for a in list
                .iter()
                .filter(|a| a.matches_action(t) && a.status == "pending")
            {
                if let Err(err) = ctx.paperclip.reject_approval(&a.id, REJECT_NOTE).await {
                    tracing::warn!(delivery_id = d.id, class = %err.class(), "cannot reject linked approval");
                }
            }
        }
        Err(err) => {
            tracing::warn!(delivery_id = d.id, class = %err.class(), "cannot list approvals")
        }
    }
}

/// Final line and delivery status for our own resolved decision.
pub fn outcome_line(
    seen: &Interaction,
    current: &Interaction,
    r: &Resolution,
) -> (DeliveryStatus, String) {
    if let Resolution::GovernedReject { .. } = r {
        return (DeliveryStatus::Answered, "✅ Отклонено".into());
    }
    let result = current.result.clone().unwrap_or_default();
    match &seen.body {
        InteractionBody::SecretProposal(_) => match result.secret_proposal {
            Some(g) if g.status == "failed" => (
                DeliveryStatus::Answered,
                format!(
                    "❌ Привязка не создана: {}. Решите в Paperclip",
                    g.error_code.as_deref().unwrap_or("ошибка")
                ),
            ),
            Some(g) if g.status == "executed" => {
                (DeliveryStatus::Answered, "✅ Секрет привязан".into())
            }
            _ => (DeliveryStatus::Answered, "✅ Привязка разрешена".into()),
        },
        _ => tool_result_line(result.tool_action.as_ref()),
    }
}

pub fn tool_result_line(g: Option<&GovernedResult>) -> (DeliveryStatus, String) {
    match g.map(|g| g.status.as_str()) {
        Some("executed") => {
            let summary = g
                .and_then(|g| g.result_summary.as_deref())
                .map(|s| format!(": {}", truncate_chars(s, RESULT_SUMMARY_MAX)))
                .unwrap_or_default();
            (DeliveryStatus::Answered, format!("✅ Выполнено{summary}"))
        }
        Some("failed") => (
            DeliveryStatus::Answered,
            format!(
                "❌ Ошибка выполнения: {}",
                g.and_then(|g| g.error_code.as_deref())
                    .unwrap_or("неизвестно")
            ),
        ),
        Some("expired") => (DeliveryStatus::Closed, "⌛ Истекло".into()),
        Some("rejected") => (DeliveryStatus::Closed, "— Отклонено".into()),
        _ => (
            DeliveryStatus::Following,
            "⏳ Разрешено, выполняется…".into(),
        ),
    }
}

async fn close<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    line: String,
) -> Result<()> {
    ctx.store
        .outbox_abandon(e.id, d.id, DeliveryStatus::Closed, Some(line))
        .await?;
    cards::refresh(ctx, d.id).await
}

/// Follows an accepted action until `executed` / `failed` / `expired`, at
/// most `GOVERNED_RESULT_FOLLOW` (called by sync for `following` cards).
pub async fn follow<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    i: &Interaction,
) -> Result<()> {
    let g = i.result.as_ref().and_then(|r| r.tool_action.as_ref());
    let (status, line) = tool_result_line(g);
    if status != DeliveryStatus::Following {
        tracing::info!(
            delivery_id = d.id,
            outcome = status.as_str(),
            "governed result received"
        );
        ctx.store.close_delivery(d.id, status, line).await?;
        return cards::refresh(ctx, d.id).await;
    }
    let overdue = d
        .follow_until
        .as_deref()
        .and_then(timeutil::parse)
        .is_some_and(|t| t <= timeutil::now());
    if overdue {
        let line = format!(
            "⏳ Результат не получен, проверьте в Paperclip: {}",
            escape(&d.issue_ref)
        );
        ctx.store
            .close_delivery(d.id, DeliveryStatus::Answered, line)
            .await?;
        return cards::refresh(ctx, d.id).await;
    }
    Ok(())
}
