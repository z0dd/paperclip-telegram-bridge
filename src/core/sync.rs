//! Paperclip → Telegram (architecture §4.4): attention feed → interactions of
//! changed issues → deliveries; closes cards resolved elsewhere.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{Ctx, Result, cards};
use crate::domain::{AttentionItem, Interaction, RunFailure};
use crate::paperclip::{PaperclipApi, PaperclipError};
use crate::render::cards::resolved_elsewhere_line;
use crate::retry::Backoff;
use crate::store::{Delivery, DeliveryStatus, NewDelivery, Snapshot};
use crate::telegram::TelegramApi;
use crate::timeutil;

/// More new cards than this in one cycle → one summary message first.
pub const SUMMARY_THRESHOLD: usize = 10;
/// Consecutive failures before "Paperclip недоступен" is sent.
pub const DOWN_NOTIFY_AFTER: u32 = 3;
pub const AUTH_RETRY_INTERVAL: Duration = Duration::from_secs(300);

struct IssueGroup {
    company_id: String,
    issue_ref: Option<String>,
    issue_title: Option<String>,
    interaction_ids: HashSet<String>,
    signature: String,
}

pub async fn run<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, cancel: CancellationToken) {
    let backoff = Backoff::from_config(&ctx.cfg.retry);
    loop {
        let delay = match cycle(&ctx).await {
            Ok(()) => ctx.cfg.paperclip.poll_interval,
            Err(super::CoreError::Paperclip(PaperclipError::Auth { .. })) => AUTH_RETRY_INTERVAL,
            Err(e) => {
                let failures = ctx
                    .shared
                    .paperclip
                    .lock()
                    .map(|s| s.consecutive_failures)
                    .unwrap_or(1);
                tracing::warn!(error = %e, failures, "sync cycle failed");
                backoff.delay(failures).max(Duration::from_secs(1))
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// Records a failed Paperclip call; sends the degraded-state messages once.
pub async fn paperclip_failed<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    e: &PaperclipError,
) {
    let mut notify = None;
    if let Ok(mut s) = ctx.shared.paperclip.lock() {
        if let PaperclipError::Auth { .. } = e {
            s.auth_failed = true;
            ctx.health.paperclip_auth_failed();
            if !s.auth_notified {
                s.auth_notified = true;
                notify = Some(
                    "🔑 Ключ Paperclip отклонён (401/403), нужна ротация ключа — см. runbook."
                        .to_string(),
                );
            }
        } else if e.is_transient() || matches!(e, PaperclipError::Decode) {
            s.consecutive_failures += 1;
            if s.down_since.is_none() {
                s.down_since = Some(timeutil::now());
            }
            if s.consecutive_failures >= DOWN_NOTIFY_AFTER && !s.down_notified {
                s.down_notified = true;
                let since = s.down_since.map(timeutil::hhmm).unwrap_or_default();
                notify = Some(format!("⚠️ Paperclip недоступен с {since}"));
            }
        }
    }
    if let Some(text) = notify {
        tracing::warn!(class = %e.class(), "paperclip degraded");
        cards::notify(ctx, text).await;
    }
}

/// Records a successful Paperclip call; announces recovery once.
pub async fn paperclip_ok<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) {
    ctx.health.paperclip_ok();
    let mut notify = None;
    if let Ok(mut s) = ctx.shared.paperclip.lock() {
        if s.down_notified || s.auth_notified {
            notify = Some("✅ Связь с Paperclip восстановлена".to_string());
        }
        *s = super::PaperclipState::default();
    }
    if let Some(text) = notify {
        tracing::info!("paperclip recovered");
        cards::notify(ctx, text).await;
    }
}

/// One sync cycle. Public for tests.
pub async fn cycle<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) -> Result<()> {
    // Cards that were stored but not (fully) sent: Telegram was down or the
    // process crashed between send and commit.
    for d in ctx
        .store
        .deliveries_with_status(vec![DeliveryStatus::New, DeliveryStatus::Sending])
        .await?
    {
        if let Err(e) = cards::deliver(ctx, d.id).await {
            tracing::warn!(delivery_id = d.id, error = %e, "redelivery failed");
            break;
        }
    }

    let mut groups: HashMap<String, IssueGroup> = HashMap::new();
    let mut failed_runs = Vec::new();
    for company in &ctx.cfg.paperclip.company_ids {
        let items = match ctx.paperclip.attention(company).await {
            Ok(items) => items,
            Err(e) => {
                paperclip_failed(ctx, &e).await;
                return Err(e.into());
            }
        };
        for item in items {
            match item {
                AttentionItem::Interaction {
                    interaction_id,
                    issue_id,
                    issue_ref,
                    issue_title,
                    activity_at,
                } => {
                    ctx.remember_issue(&issue_id, issue_ref.as_deref(), issue_title.as_deref());
                    let g = groups.entry(issue_id).or_insert_with(|| IssueGroup {
                        company_id: company.clone(),
                        issue_ref,
                        issue_title,
                        interaction_ids: HashSet::new(),
                        signature: String::new(),
                    });
                    g.signature.push_str(&format!(
                        "{interaction_id}@{};",
                        activity_at.unwrap_or_default()
                    ));
                    g.interaction_ids.insert(interaction_id);
                }
                AttentionItem::FailedRun {
                    run_id,
                    agent_id,
                    issue_id,
                    issue_ref,
                    error_code,
                    activity_at,
                } => failed_runs.push(RunFailure {
                    run_id,
                    company_id: company.clone(),
                    agent_id,
                    issue_id,
                    issue_ref,
                    status: "failed".into(),
                    error_code,
                    at: activity_at,
                }),
                AttentionItem::Other => {}
            }
        }
    }
    paperclip_ok(ctx).await;
    if let Ok(mut q) = ctx.shared.failed_runs.lock() {
        q.extend(failed_runs);
    }

    let feed_ids: HashSet<String> = groups
        .values()
        .flat_map(|g| g.interaction_ids.iter().cloned())
        .collect();
    let known: HashSet<String> = ctx
        .store
        .known_interactions(feed_ids.iter().cloned().collect())
        .await?
        .into_iter()
        .collect();

    // Issues to read: new or changed feed entries…
    let mut to_fetch: HashMap<String, (String, Option<String>, Option<String>)> = HashMap::new();
    {
        let prev = ctx
            .shared
            .issue_activity
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        for (issue_id, g) in &groups {
            let changed = prev.get(issue_id) != Some(&g.signature);
            if changed || g.interaction_ids.iter().any(|id| !known.contains(id)) {
                to_fetch.insert(
                    issue_id.clone(),
                    (
                        g.company_id.clone(),
                        g.issue_ref.clone(),
                        g.issue_title.clone(),
                    ),
                );
            }
        }
    }
    // …and open cards that left the feed (resolved elsewhere or collapsed),
    // plus governed cards whose result is still being followed.
    let open = ctx
        .store
        .deliveries_with_status(vec![
            DeliveryStatus::Sent,
            DeliveryStatus::Answering,
            DeliveryStatus::Unsupported,
            DeliveryStatus::Following,
        ])
        .await?;
    for d in &open {
        if d.status == DeliveryStatus::Following || !feed_ids.contains(&d.interaction_id) {
            to_fetch.entry(d.issue_id.clone()).or_insert_with(|| {
                (
                    d.company_id.clone(),
                    Some(d.issue_ref.clone()),
                    d.snapshot.issue_title.clone(),
                )
            });
        }
    }

    let mut candidates: Vec<(String, String, Option<String>, Interaction)> = Vec::new();
    for (issue_id, (company_id, issue_ref, issue_title)) in to_fetch {
        let list = match ctx.paperclip.interactions(&issue_id).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(class = %e.class(), "cannot read issue interactions");
                if matches!(e, PaperclipError::Auth { .. }) {
                    paperclip_failed(ctx, &e).await;
                    return Err(e.into());
                }
                continue;
            }
        };
        let issue_ref = issue_ref
            .or_else(|| ctx.issue_ref(&issue_id))
            .unwrap_or_else(|| short_id(&issue_id));
        let self_id = ctx.self_user_id();
        for i in &list {
            if !i.status.is_pending() || known.contains(&i.id) {
                continue;
            }
            let in_feed = feed_ids.contains(&i.id);
            if !in_feed && !addressed_to_us(i, self_id.as_deref()) {
                continue;
            }
            if ctx
                .store
                .delivery_by_interaction(i.id.clone())
                .await?
                .is_some()
            {
                continue;
            }
            candidates.push((
                company_id.clone(),
                issue_ref.clone(),
                issue_title.clone(),
                i.clone(),
            ));
        }
        for d in open.iter().filter(|d| d.issue_id == issue_id) {
            reconcile_open(ctx, d, list.iter().find(|i| i.id == d.interaction_id)).await?;
        }
        if let (Some(g), Ok(mut m)) = (groups.get(&issue_id), ctx.shared.issue_activity.lock()) {
            m.insert(issue_id.clone(), g.signature.clone());
        }
    }

    candidates.sort_by(|a, b| a.3.created_at.cmp(&b.3.created_at));
    if candidates.len() > SUMMARY_THRESHOLD {
        cards::notify(ctx, format!("📬 Новых вопросов: {}", candidates.len())).await;
    }
    for (company_id, issue_ref, issue_title, i) in candidates {
        let agent_name = ctx
            .agent_name(&company_id, i.created_by_agent_id.as_deref())
            .await;
        let new = NewDelivery {
            interaction_id: i.id.clone(),
            company_id,
            issue_id: i.issue_id.clone(),
            issue_ref,
            snapshot: Snapshot {
                interaction: i,
                issue_title,
                agent_name,
            },
            status: DeliveryStatus::New,
        };
        if let Some(id) = ctx.store.insert_delivery(new).await?
            && let Err(e) = cards::deliver(ctx, id).await
        {
            // Stays new/sending and is retried next cycle.
            tracing::warn!(delivery_id = id, error = %e, "delivery failed");
        }
    }
    Ok(())
}

/// Cards not in the feed are delivered only if nobody else is the addressee.
fn addressed_to_us(i: &Interaction, self_id: Option<&str>) -> bool {
    if i.addressee_agent_id.is_some() {
        return false;
    }
    match (&i.addressee_user_id, self_id) {
        (None, _) => true,
        (Some(a), Some(me)) => a == me,
        (Some(_), None) => false,
    }
}

pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Brings an open delivery in line with the current card state.
async fn reconcile_open<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    current: Option<&Interaction>,
) -> Result<()> {
    match (d.status, current) {
        // The outbox owns cards with an answer in flight.
        (DeliveryStatus::Answering, _) => Ok(()),
        (DeliveryStatus::Following, Some(i)) => super::governed::follow(ctx, d, i).await,
        (DeliveryStatus::Following, None) => {
            ctx.store
                .close_delivery(d.id, DeliveryStatus::Closed, "— Решено в Paperclip".into())
                .await?;
            cards::refresh(ctx, d.id).await
        }
        (_, Some(i)) if i.status.is_pending() => Ok(()),
        (_, current) => {
            let line = current
                .map(resolved_elsewhere_line)
                .unwrap_or_else(|| "— Решено в Paperclip: устарело".into());
            ctx.store
                .close_delivery(d.id, DeliveryStatus::Closed, line)
                .await?;
            if let Some(i) = current {
                let mut snapshot = d.snapshot.clone();
                snapshot.interaction.status = i.status.clone();
                snapshot.interaction.result = i.result.clone();
                ctx.store.update_snapshot(d.id, snapshot).await?;
            }
            tracing::info!(
                delivery_id = d.id,
                interaction_id = %d.interaction_id,
                status = current.map(|i| i.status.as_str()).unwrap_or("missing"),
                "card resolved elsewhere"
            );
            if ctx.cfg.notify.resolved_elsewhere {
                cards::refresh(ctx, d.id).await?;
            }
            Ok(())
        }
    }
}
