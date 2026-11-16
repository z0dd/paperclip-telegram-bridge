//! Telegram → Paperclip: sends queued answers, retries transient failures
//! forever (an answer is never dropped), reconciles `409` by card state
//! (architecture §4.4, API §7).

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{CoreError, Ctx, Result, cards, governed};
use crate::domain::*;
use crate::fault;
use crate::paperclip::{PaperclipApi, PaperclipError};
use crate::render::{cards::resolved_elsewhere_line, escape};
use crate::retry::Backoff;
use crate::store::{Delivery, DeliveryStatus, OutboxEntry};
use crate::telegram::TelegramApi;
use crate::timeutil;

const IDLE_WAIT: Duration = Duration::from_secs(60);

pub async fn run<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, cancel: CancellationToken) {
    loop {
        if let Err(e) = process_due(&ctx, &cancel).await {
            tracing::warn!(error = %e, "outbox pass failed");
        }
        let wait = match ctx.store.next_outbox_due().await {
            Ok(Some(at)) => timeutil::parse(&at)
                .map(|t| (t - timeutil::now()).max(time::Duration::ZERO))
                .and_then(|d| Duration::try_from(d).ok())
                .unwrap_or(Duration::from_secs(1))
                .min(IDLE_WAIT),
            _ => IDLE_WAIT,
        };
        let notified = ctx.outbox_notify.notified();
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = notified => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// Processes all due entries. Stops taking new ones once shutdown starts;
/// the entry in flight is finished (bounded by `SHUTDOWN_GRACE` in `main`).
pub async fn process_due<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    cancel: &CancellationToken,
) -> Result<()> {
    for entry in ctx.store.due_outbox(timeutil::now_string()).await? {
        if cancel.is_cancelled() {
            break;
        }
        process(ctx, entry).await?;
    }
    Ok(())
}

async fn process<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>, e: OutboxEntry) -> Result<()> {
    let Some(d) = ctx.store.delivery(e.delivery_id).await? else {
        return Ok(());
    };
    let Some(resolution) = e.resolution.clone() else {
        ctx.store
            .outbox_abandon(
                e.id,
                d.id,
                DeliveryStatus::Closed,
                Some("— Ответ потерян".into()),
            )
            .await?;
        return Ok(());
    };
    ctx.store.outbox_in_flight(e.id).await?;
    if resolution.is_governed() {
        return governed::process(ctx, &d, &e, &resolution).await;
    }
    let result = ctx
        .paperclip
        .resolve(&d.issue_id, &d.interaction_id, &resolution)
        .await;
    if fault::hit(fault::AFTER_HTTP_BEFORE_OUTBOX_COMMIT) {
        return Err(CoreError::Fault);
    }
    match result {
        Ok(record) => {
            super::sync::paperclip_ok(ctx).await;
            if let Some(uid) = record
                .as_ref()
                .and_then(|r| r.resolved_by_user_id.as_deref())
            {
                ctx.learn_self_user_id(uid).await;
            }
            // Partial verdicts keep the card open (API §6.4).
            if let Resolution::Verdicts { .. } = &resolution {
                let current = match record {
                    Some(rec) => Some(rec),
                    // No record in the response: read the card to tell
                    // partial from complete.
                    None => ctx
                        .paperclip
                        .interactions(&d.issue_id)
                        .await
                        .ok()
                        .and_then(|l| l.into_iter().find(|i| i.id == d.interaction_id)),
                };
                if let Some(rec) = current.filter(|r| r.status.is_pending()) {
                    return super::forms::verdicts_partial_done(ctx, &d, &e, &rec).await;
                }
            }
            succeed(ctx, &d, &e, &resolution).await
        }
        Err(PaperclipError::Resolved { code }) => {
            tracing::info!(delivery_id = d.id, code = %code, "answer conflicts, reconciling");
            reconcile(ctx, &d, &e, &resolution).await
        }
        Err(PaperclipError::Invalid {
            status, message, ..
        }) => {
            tracing::warn!(
                delivery_id = d.id,
                status,
                "answer rejected by Paperclip validation"
            );
            ctx.store
                .outbox_remove(e.id, d.id, DeliveryStatus::Sent)
                .await?;
            ctx.store
                .apply(vec![crate::store::StoreOp::BumpGeneration(d.id)])
                .await?;
            let reason = if message.is_empty() {
                String::new()
            } else {
                format!(": {}", escape(&message))
            };
            cards::notify(
                ctx,
                format!(
                    "❌ Paperclip не принял ответ на {}{reason}\nИсправьте ответ и отправьте снова или ответьте в Paperclip.",
                    escape(&d.issue_ref)
                ),
            )
            .await;
            cards::refresh(ctx, d.id).await
        }
        Err(PaperclipError::AudienceDenied { code }) => {
            tracing::warn!(delivery_id = d.id, code = %code, "key may not answer this card");
            close_with(
                ctx,
                &d,
                &e,
                "— Paperclip не разрешает этому ключу ответить на эту карточку, ответьте в UI"
                    .into(),
            )
            .await
        }
        Err(PaperclipError::NotFound) => {
            close_with(ctx, &d, &e, "— Карточка не найдена в Paperclip".into()).await
        }
        Err(err) => retry_later(ctx, &d, &e, &err).await,
    }
}

/// Text appended to the card after a successful answer.
pub fn success_line(d: &Delivery, r: &Resolution) -> String {
    let i = &d.snapshot.interaction;
    match (&i.body, r) {
        (InteractionBody::Confirmation(c), Resolution::Accept { .. }) => format!(
            "✅ Ответ отправлен: {}",
            c.accept_label.as_deref().unwrap_or("Approve")
        ),
        (InteractionBody::Confirmation(c), Resolution::Reject { .. }) => format!(
            "✅ Ответ отправлен: {}",
            c.reject_label.as_deref().unwrap_or("Reject")
        ),
        (
            InteractionBody::Checkbox(_),
            Resolution::Accept {
                selected_option_ids,
            },
        ) => format!(
            "✅ Ответ отправлен: выбрано {}",
            selected_option_ids.as_ref().map_or(0, Vec::len)
        ),
        (InteractionBody::Checkbox(c), Resolution::Reject { .. }) => format!(
            "✅ Ответ отправлен: {}",
            c.reject_label.as_deref().unwrap_or("отклонено")
        ),
        (InteractionBody::Questions(f), Resolution::Respond { answers }) => {
            super::forms::respond_summary(f, answers)
        }
        (InteractionBody::Verdicts(_), Resolution::Verdicts { .. }) => {
            "✅ Вердикты отправлены".into()
        }
        _ => "✅ Ответ отправлен".into(),
    }
}

async fn succeed<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    r: &Resolution,
) -> Result<()> {
    ctx.store
        .outbox_done(
            e.id,
            d.id,
            DeliveryStatus::Answered,
            Some(success_line(d, r)),
        )
        .await?;
    tracing::info!(
        delivery_id = d.id,
        interaction_id = %d.interaction_id,
        action = r.action_name(),
        outcome = "sent",
        "answer delivered to Paperclip"
    );
    cards::refresh(ctx, d.id).await
}

async fn close_with<P: PaperclipApi, T: TelegramApi>(
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

/// Is the stored result the answer we sent? (API §7: our earlier request
/// reached the server but the response was lost.)
pub fn result_matches(i: &Interaction, r: &Resolution) -> bool {
    let res = i.result.clone().unwrap_or_default();
    match r {
        Resolution::Accept {
            selected_option_ids,
        } => {
            i.status == InteractionStatus::Accepted
                && match (selected_option_ids, &res.selected_option_ids) {
                    (Some(a), Some(b)) => sorted(a) == sorted(b),
                    (Some(_), None) => true,
                    (None, _) => true,
                }
        }
        Resolution::Reject { .. } => i.status == InteractionStatus::Rejected,
        Resolution::Respond { answers } => {
            i.status == InteractionStatus::Answered
                && res.answers.as_ref().is_some_and(|stored| {
                    let norm = |v: &[QuestionAnswer]| {
                        let mut v: Vec<(String, Vec<String>, Option<String>)> = v
                            .iter()
                            .map(|a| {
                                (
                                    a.question_id.clone(),
                                    sorted(&a.option_ids),
                                    a.other_text.clone(),
                                )
                            })
                            .collect();
                        v.sort();
                        v
                    };
                    norm(stored) == norm(answers)
                })
        }
        Resolution::Verdicts { verdicts } => match &i.body {
            InteractionBody::Verdicts(v) => verdicts.iter().all(|ours| {
                v.decided
                    .iter()
                    .any(|(id, vd)| *id == ours.id && *vd == ours.verdict)
            }),
            _ => false,
        },
        Resolution::GovernedAccept { .. } => i.status == InteractionStatus::Accepted,
        Resolution::GovernedReject { .. } => i.status == InteractionStatus::Rejected,
    }
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v = v.to_vec();
    v.sort();
    v
}

/// The card was resolved by the key owner (not by an agent).
pub fn resolved_by_us(i: &Interaction, self_user_id: Option<&str>) -> bool {
    match (self_user_id, i.resolved_by_user_id.as_deref()) {
        (Some(me), Some(by)) => me == by,
        // Owner not learned yet: a user (not an agent) resolved it.
        (None, Some(_)) => i.resolved_by_agent_id.is_none(),
        _ => false,
    }
}

async fn reconcile<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    r: &Resolution,
) -> Result<()> {
    let list = match ctx.paperclip.interactions(&d.issue_id).await {
        Ok(l) => l,
        Err(err) => return retry_later(ctx, d, e, &err).await,
    };
    let Some(current) = list.into_iter().find(|i| i.id == d.interaction_id) else {
        return close_with(
            ctx,
            d,
            e,
            "— Решено в Paperclip: карточка не найдена".into(),
        )
        .await;
    };
    if current.status.is_pending() {
        // 409 but still pending (e.g. a transient conflict): try again later.
        return retry_later(ctx, d, e, &PaperclipError::Conflict).await;
    }
    if result_matches(&current, r) && resolved_by_us(&current, ctx.self_user_id().as_deref()) {
        return succeed(ctx, d, e, r).await;
    }
    let line = format!(
        "{} — ваш ответ не отправлен",
        resolved_elsewhere_line(&current)
    );
    tracing::info!(
        delivery_id = d.id,
        interaction_id = %d.interaction_id,
        status = current.status.as_str(),
        outcome = "resolved_elsewhere",
        "answer not sent"
    );
    ctx.store
        .outbox_abandon(e.id, d.id, DeliveryStatus::Closed, Some(line))
        .await?;
    let mut snapshot = d.snapshot.clone();
    snapshot.interaction.status = current.status.clone();
    snapshot.interaction.result = current.result.clone();
    ctx.store.update_snapshot(d.id, snapshot).await?;
    cards::refresh(ctx, d.id).await
}

/// Transient failure: keep the answer, back off, tell the user once.
pub async fn retry_later<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    err: &PaperclipError,
) -> Result<()> {
    if matches!(err, PaperclipError::Auth { .. }) || err.is_transient() {
        super::sync::paperclip_failed(ctx, err).await;
    }
    let backoff = Backoff::from_config(&ctx.cfg.retry);
    let rate_limited = err.retry_after();
    let attempts = if rate_limited.is_some() {
        e.attempts
    } else {
        e.attempts + 1
    };
    let delay = match (err, rate_limited) {
        (_, Some(ra)) => ra + crate::retry::rate_limit_jitter(),
        (PaperclipError::Auth { .. }, _) => super::sync::AUTH_RETRY_INTERVAL,
        _ => backoff.delay(attempts),
    };
    let next = timeutil::format(timeutil::add(timeutil::now(), delay));
    ctx.store
        .outbox_retry(e.id, attempts, next, err.class())
        .await?;
    tracing::warn!(
        delivery_id = d.id,
        class = %err.class(),
        attempts,
        "answer not delivered yet, will retry"
    );
    if attempts == ctx.cfg.retry.notify_after && e.attempts < attempts {
        cards::notify(
            ctx,
            format!(
                "⏳ Paperclip недоступен, ответ на {} в очереди и будет отправлен автоматически.",
                escape(&d.issue_ref)
            ),
        )
        .await;
    }
    Ok(())
}
