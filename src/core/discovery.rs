//! Discovery of companies and conversations (ADR-0010 §2.2, §2.3).
//!
//! Every `PAPERCLIP_DISCOVERY_INTERVAL`: companies of every key and the agent
//! names. Every `PAPERCLIP_POLL_INTERVAL`: `/chats` of each company. A new
//! conversation gets its watermark and a topic job; a vanished one is marked
//! `removed_at`. Comments are read by [`super::outbound`] in the same pass.
//! Each company has its own backoff: one failing company does not slow the
//! others.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{AUTH_RETRY_INTERVAL, Ctx, Result, outbound};
use crate::domain::{Chat, Company};
use crate::paperclip::{PaperclipApi, PaperclipError};
use crate::render::{self, notices};
use crate::retry::Backoff;
use crate::store::{CompanyRow, Conversation, NewConversation};
use crate::telegram::TelegramApi;
use crate::timeutil;

/// `seen_comments` are kept this long, or the newest [`SEEN_KEEP`] per
/// conversation (ADR-0010 §2.4).
pub const SEEN_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);
pub const SEEN_KEEP: u32 = 500;
const PURGE_INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
pub struct TrackedCompany {
    pub company: Company,
    /// Key that sees the company (the first one in configuration order).
    pub key: usize,
    pub next_try: Instant,
    pub failures: u32,
    /// `403`/`404` on `/chats`: skipped until the next discovery.
    pub unavailable: bool,
}

#[derive(Debug, Default)]
pub struct DiscoveryState {
    pub companies: Vec<TrackedCompany>,
    last_discovery: Option<Instant>,
    last_purge: Option<Instant>,
}

impl DiscoveryState {
    /// The next cycle discovers companies and agents again.
    pub fn force_discovery(&mut self) {
        self.last_discovery = None;
    }

    /// Clears per-company backoff (tests).
    pub fn reset_backoff(&mut self) {
        let now = Instant::now();
        for c in &mut self.companies {
            c.next_try = now;
        }
    }
}

pub async fn run<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, cancel: CancellationToken) {
    let mut state = DiscoveryState::default();
    loop {
        if let Err(e) = cycle(&ctx, &mut state).await {
            tracing::warn!(error = %e, "discovery cycle failed");
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(ctx.cfg.paperclip.poll_interval) => {}
        }
    }
}

/// One pass: discovery when due, then `/chats` and comments of every company.
pub async fn cycle<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    state: &mut DiscoveryState,
) -> Result<()> {
    let due = state
        .last_discovery
        .is_none_or(|t| t.elapsed() >= ctx.cfg.paperclip.discovery_interval);
    if due {
        discover(ctx, state).await?;
        state.last_discovery = Some(Instant::now());
    }
    if state
        .last_purge
        .is_none_or(|t| t.elapsed() >= PURGE_INTERVAL)
    {
        let before = timeutil::format(timeutil::now() - SEEN_RETENTION);
        let purged = ctx.store.purge_seen(before, SEEN_KEEP).await?;
        if purged > 0 {
            tracing::debug!(purged, "old seen comments purged");
        }
        state.last_purge = Some(Instant::now());
    }
    let backoff = Backoff::from_config(&ctx.cfg.retry);
    for i in 0..state.companies.len() {
        let tc = &state.companies[i];
        if tc.unavailable || tc.next_try > Instant::now() {
            continue;
        }
        let (company, key) = (tc.company.clone(), tc.key);
        let delay = match ctx.key(key).chats(&company.id).await {
            Ok(chats) => {
                ctx.paperclip_ok(key);
                agent_chat_ok(ctx);
                state.companies[i].failures = 0;
                sync_company(ctx, &company, key, chats).await?;
                None
            }
            Err(e) => chats_failed(ctx, &mut state.companies[i], &e, &backoff),
        };
        if let Some(d) = delay {
            state.companies[i].next_try = Instant::now() + d;
        }
    }
    let depth = ctx.store.queue_depth().await?;
    ctx.health.set_queue_depth(depth);
    Ok(())
}

/// Companies of every key, merged by id (first key wins), and agent names.
async fn discover<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    state: &mut DiscoveryState,
) -> Result<()> {
    let mut merged: Vec<(Company, usize)> = Vec::new();
    let mut all_ok = true;
    for (i, key) in ctx.keys.iter().enumerate() {
        match key.companies().await {
            Ok(list) => {
                ctx.paperclip_ok(i);
                for c in list.into_iter().filter(Company::is_active) {
                    if !merged.iter().any(|(m, _)| m.id == c.id) {
                        merged.push((c, i));
                    }
                }
            }
            Err(e) => {
                all_ok = false;
                if matches!(e, PaperclipError::Auth) {
                    ctx.key_rejected(i);
                } else {
                    tracing::warn!(key = i, class = %e.class(), "company list unavailable");
                }
                // Keep what this key saw before.
                for tc in state.companies.iter().filter(|tc| tc.key == i) {
                    if !merged.iter().any(|(m, _)| m.id == tc.company.id) {
                        merged.push((tc.company.clone(), i));
                    }
                }
            }
        }
    }
    for (c, key) in &merged {
        match ctx.key(*key).agents(&c.id).await {
            Ok(agents) => {
                let names: HashMap<String, String> =
                    agents.into_iter().map(|a| (a.id, a.name)).collect();
                if let Ok(mut m) = ctx.shared.agents.lock() {
                    m.insert(c.id.clone(), names);
                }
            }
            Err(e) => {
                all_ok = false;
                tracing::debug!(company_id = %c.id, class = %e.class(), "agent list unavailable");
            }
        }
    }
    ctx.store
        .upsert_companies(
            merged
                .iter()
                .map(|(c, key)| CompanyRow {
                    company_id: c.id.clone(),
                    name: c.name.clone(),
                    issue_prefix: c.issue_prefix.clone(),
                    key_ref: ctx.key_refs[*key].clone(),
                })
                .collect(),
        )
        .await?;
    let now = Instant::now();
    let previous = std::mem::take(&mut state.companies);
    state.companies = merged
        .into_iter()
        .map(|(company, key)| {
            let old = previous.iter().find(|p| p.company.id == company.id);
            TrackedCompany {
                key,
                next_try: old.map_or(now, |o| o.next_try),
                failures: old.map_or(0, |o| o.failures),
                unavailable: false,
                company,
            }
        })
        .collect();
    let added: Vec<&str> = state
        .companies
        .iter()
        .filter(|c| !previous.iter().any(|p| p.company.id == c.company.id))
        .map(|c| c.company.issue_prefix.as_str())
        .collect();
    if !added.is_empty() {
        tracing::info!(companies = ?added, total = state.companies.len(), "companies discovered");
    }
    if all_ok {
        ctx.health.discovery_ok();
    }
    Ok(())
}

fn agent_chat_ok<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) {
    ctx.health.set_agent_chat_ok(true);
    if let Ok(mut n) = ctx.shared.agent_chat_notified.lock()
        && std::mem::replace(&mut *n, false)
    {
        tracing::info!("agent chat is available again");
    }
}

/// Classifies a `/chats` failure; returns the delay before the next try.
fn chats_failed<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    tc: &mut TrackedCompany,
    e: &PaperclipError,
    backoff: &Backoff,
) -> Option<Duration> {
    let company = &tc.company;
    if e.is_agent_chat_disabled() {
        ctx.health.set_agent_chat_ok(false);
        let first = ctx
            .shared
            .agent_chat_notified
            .lock()
            .map(|mut n| !std::mem::replace(&mut *n, true))
            .unwrap_or(false);
        if first {
            tracing::error!(company_id = %company.id, "Agent Chat is disabled in Paperclip");
            ctx.notify(None, notices::AGENT_CHAT_DISABLED);
        }
        return Some(ctx.cfg.paperclip.discovery_interval);
    }
    match e {
        PaperclipError::Auth => {
            ctx.key_rejected(tc.key);
            tc.failures += 1;
            Some(AUTH_RETRY_INTERVAL)
        }
        PaperclipError::Forbidden { .. } | PaperclipError::NotFound { .. } => {
            tracing::warn!(
                company_id = %company.id,
                class = %e.class(),
                "conversations of the company are not available, skipped until the next discovery"
            );
            tc.unavailable = true;
            None
        }
        e => {
            tc.failures += 1;
            tracing::warn!(
                company_id = %company.id,
                class = %e.class(),
                failures = tc.failures,
                "conversation list failed"
            );
            Some(
                e.retry_after()
                    .map(|ra| ra + crate::retry::rate_limit_jitter())
                    .unwrap_or_else(|| backoff.delay(tc.failures)),
            )
        }
    }
}

/// Is this conversation owned by the same Paperclip user as the others?
/// All keys must belong to one user (ADR-0010 §1.1).
fn owner_matches<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    chat: &Chat,
    key: usize,
) -> bool {
    let Some(user) = chat.user_id.as_deref() else {
        return true;
    };
    let Ok(mut owner) = ctx.shared.owner.lock() else {
        return true;
    };
    match owner.as_ref() {
        None => {
            *owner = Some((user.to_string(), key));
            true
        }
        Some((u, _)) if u == user => true,
        Some((_, first_key)) => {
            tracing::error!(
                key,
                first_key,
                issue_id = %chat.issue_id,
                "keys belong to different Paperclip users: conversation skipped, use keys of one user"
            );
            ctx.health.set_paperclip_auth_ok(false);
            false
        }
    }
}

async fn sync_company<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    company: &Company,
    key: usize,
    chats: Vec<Chat>,
) -> Result<()> {
    let known: HashMap<String, Conversation> = ctx
        .store
        .conversations()
        .await?
        .into_iter()
        .filter(|c| c.company_id == company.id)
        .map(|c| (c.issue_id.clone(), c))
        .collect();
    let mut present = HashSet::new();
    for chat in chats {
        if !owner_matches(ctx, &chat, key) {
            continue;
        }
        present.insert(chat.issue_id.clone());
        match known.get(&chat.issue_id) {
            None => discover_conversation(ctx, company, key, &chat).await?,
            Some(conv) => {
                if conv.removed_at.is_some() {
                    tracing::info!(issue_id = %conv.issue_id, "conversation is back");
                    ctx.store.set_removed(conv.issue_id.clone(), None).await?;
                    if conv.topic_state != crate::store::TopicState::Active {
                        ctx.sender_notify.notify_one();
                    }
                }
                let changed = chat.updated_at.is_some() && chat.updated_at != conv.last_updated_at;
                if changed {
                    ctx.store
                        .set_updated_at(
                            conv.issue_id.clone(),
                            chat.updated_at.clone(),
                            ctx.hot_until(),
                        )
                        .await?;
                }
                let due = changed || is_hot(conv) || sweep_due(ctx, conv);
                if due && let Err(e) = outbound::read(ctx, key, conv, changed).await {
                    match e {
                        super::CoreError::Paperclip(pe) => {
                            tracing::warn!(issue_id = %conv.issue_id, class = %pe.class(), "comments not read")
                        }
                        e => return Err(e),
                    }
                }
            }
        }
    }
    let now = timeutil::now_string();
    for conv in known.values() {
        if conv.removed_at.is_none() && !present.contains(&conv.issue_id) {
            tracing::info!(issue_id = %conv.issue_id, "conversation no longer listed");
            ctx.store
                .set_removed(conv.issue_id.clone(), Some(now.clone()))
                .await?;
        }
    }
    Ok(())
}

fn is_hot(conv: &Conversation) -> bool {
    conv.hot_until
        .as_deref()
        .is_some_and(|h| h > timeutil::now_string().as_str())
}

fn sweep_due<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>, conv: &Conversation) -> bool {
    match conv.last_swept_at.as_deref().and_then(timeutil::parse) {
        Some(t) => timeutil::add(t, ctx.cfg.paperclip.sweep_interval) <= timeutil::now(),
        None => true,
    }
}

/// New conversation: watermark (newest comment now) and a topic job.
async fn discover_conversation<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    company: &Company,
    key: usize,
    chat: &Chat,
) -> Result<()> {
    let watermark = match ctx.key(key).comments_latest(&chat.issue_id, 1).await {
        Ok(list) => list.into_iter().next().map(|c| c.id),
        Err(e) => {
            // Without a watermark the history would be forwarded: try next cycle.
            tracing::warn!(issue_id = %chat.issue_id, class = %e.class(), "watermark not read, conversation postponed");
            return Ok(());
        }
    };
    let agent_name = ctx
        .agent_name(&company.id, &chat.agent_id)
        .unwrap_or_else(|| "агент".to_string());
    let topic_name = render::topic_name(&company.issue_prefix, &agent_name);
    let inserted = ctx
        .store
        .insert_conversation(NewConversation {
            issue_id: chat.issue_id.clone(),
            company_id: company.id.clone(),
            identifier: chat.identifier.clone(),
            agent_id: chat.agent_id.clone(),
            agent_name,
            key_ref: ctx.key_refs[key].clone(),
            watermark_comment_id: watermark,
            updated_at: chat.updated_at.clone(),
            topic_name: topic_name.clone(),
        })
        .await?;
    if inserted {
        tracing::info!(
            issue_id = %chat.issue_id,
            identifier = chat.identifier.as_deref().unwrap_or("-"),
            topic = %topic_name,
            "conversation discovered"
        );
        ctx.sender_notify.notify_one();
    }
    Ok(())
}
