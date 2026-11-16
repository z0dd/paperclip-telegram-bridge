//! Agent run failures and completed tasks (architecture §8.3 g, i; API §9).
//! Only `failed` / `timed_out` are reported, only `errorCode` and status —
//! never `error` / `stderrExcerpt`.

use std::collections::HashSet;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{Ctx, Result, cards};
use crate::domain::RunFailure;
use crate::paperclip::PaperclipApi;
use crate::render::{escape, truncate_chars};
use crate::store::meta;
use crate::telegram::TelegramApi;
use crate::timeutil;

pub const RUNS_LIMIT: u32 = 100;
pub const DONE_LIMIT: u32 = 50;
pub const SEEN_RETENTION: Duration = Duration::from_secs(14 * 24 * 3600);
pub const ALL_STATUSES: [&str; 7] = [
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "blocked",
    "done",
    "cancelled",
];

pub async fn run<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, cancel: CancellationToken) {
    loop {
        if let Err(e) = cycle(&ctx).await {
            tracing::warn!(error = %e, "runs cycle failed");
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(ctx.cfg.paperclip.runs_poll_interval) => {}
        }
    }
}

/// Fills the issue identifier cache from the company issue list (once).
async fn issue_ref<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    company_id: &str,
    issue_id: Option<&str>,
    loaded: &mut HashSet<String>,
) -> Option<String> {
    let issue_id = issue_id?;
    if let Some(r) = ctx.issue_ref(issue_id) {
        return Some(r);
    }
    if loaded.insert(company_id.to_string())
        && let Ok(list) = ctx.paperclip.issues(company_id, &ALL_STATUSES, 1000).await
    {
        for i in &list {
            ctx.remember_issue(&i.id, i.identifier.as_deref(), Some(&i.title));
        }
    }
    ctx.issue_ref(issue_id)
}

pub fn failure_text(f: &RunFailure, agent: &str, issue_ref: Option<&str>) -> String {
    let mut s = format!(
        "⚠️ <b>Сбой агента {}</b>",
        escape(&truncate_chars(agent, 64))
    );
    if let Some(r) = issue_ref {
        s.push_str(&format!(" · {}", escape(r)));
    }
    s.push_str(&format!("\nСтатус: {}", escape(&f.status)));
    if let Some(code) = &f.error_code {
        s.push_str(&format!(" · код: {}", escape(&truncate_chars(code, 100))));
    }
    if let Some(t) = f.at.as_deref().and_then(timeutil::parse) {
        s.push_str(&format!("\nВремя: {}", timeutil::full(t)));
    }
    s
}

/// One cycle. Public for tests.
pub async fn cycle<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) -> Result<()> {
    let baseline = ctx.store.get_meta(meta::RUNS_BASELINE_AT).await?;
    let first = baseline.is_none();
    let baseline_at = baseline
        .as_deref()
        .and_then(timeutil::parse)
        .unwrap_or_else(timeutil::now);
    let mut loaded = HashSet::new();
    let mut to_notify: Vec<RunFailure> = Vec::new();
    let mut seen_now: Vec<String> = Vec::new();

    for company in &ctx.cfg.paperclip.company_ids {
        let runs = ctx.paperclip.heartbeat_runs(company, RUNS_LIMIT).await?;
        for r in runs.into_iter().filter(|r| r.is_failure()) {
            // A retry is scheduled: wait for the final outcome.
            if r.scheduled_retry_at.is_some() {
                continue;
            }
            if ctx.store.is_run_seen(r.id.clone()).await? {
                continue;
            }
            let finished = r
                .finished_at
                .as_deref()
                .or(r.created_at.as_deref())
                .and_then(timeutil::parse);
            let old = finished.is_none_or(|t| t < baseline_at);
            seen_now.push(r.id.clone());
            if first || old {
                continue;
            }
            to_notify.push(RunFailure {
                run_id: r.id,
                company_id: company.clone(),
                agent_id: r.agent_id,
                issue_id: r.issue_id,
                issue_ref: None,
                status: r.status,
                error_code: r.error_code,
                at: r.finished_at.or(r.created_at),
            });
        }
    }
    // Final failures from the attention feed (retries exhausted).
    let feed: Vec<RunFailure> = ctx
        .shared
        .failed_runs
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default();
    for f in feed {
        if seen_now.contains(&f.run_id) || ctx.store.is_run_seen(f.run_id.clone()).await? {
            continue;
        }
        seen_now.push(f.run_id.clone());
        if !first {
            to_notify.push(f);
        }
    }

    ctx.store.mark_runs_seen(seen_now).await?;
    if first {
        ctx.store
            .set_meta(meta::RUNS_BASELINE_AT, timeutil::format(baseline_at))
            .await?;
    }
    if ctx.cfg.notify.run_failures {
        for f in to_notify {
            let agent = ctx
                .agent_name(&f.company_id, f.agent_id.as_deref())
                .await
                .unwrap_or_else(|| crate::render::cards::DEFAULT_AGENT.into());
            let issue = match f.issue_ref.clone() {
                Some(r) => Some(r),
                None => issue_ref(ctx, &f.company_id, f.issue_id.as_deref(), &mut loaded).await,
            };
            tracing::info!(run_id = %f.run_id, status = %f.status, "run failure reported");
            cards::notify(ctx, failure_text(&f, &agent, issue.as_deref())).await;
        }
    }
    let cutoff = timeutil::format(
        timeutil::now() - time::Duration::try_from(SEEN_RETENTION).unwrap_or(time::Duration::ZERO),
    );
    ctx.store.purge_seen(cutoff).await?;

    if ctx.cfg.notify.task_completed {
        completed_tasks(ctx).await?;
    }
    Ok(())
}

async fn completed_tasks<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) -> Result<()> {
    let first = ctx.store.get_meta(meta::DONE_BASELINE_AT).await?.is_none();
    for company in &ctx.cfg.paperclip.company_ids {
        let done = ctx.paperclip.issues(company, &["done"], DONE_LIMIT).await?;
        let mut new = Vec::new();
        for i in done {
            ctx.remember_issue(&i.id, i.identifier.as_deref(), Some(&i.title));
            if !ctx.store.is_done_issue_seen(i.id.clone()).await? {
                new.push(i);
            }
        }
        ctx.store
            .mark_done_issues_seen(new.iter().map(|i| i.id.clone()).collect())
            .await?;
        if first {
            continue;
        }
        for i in new {
            let r = i.identifier.as_deref().unwrap_or("задача");
            cards::notify(
                ctx,
                format!(
                    "🏁 {} завершена: {}",
                    escape(r),
                    escape(&truncate_chars(&i.title, 200))
                ),
            )
            .await;
        }
    }
    if first {
        ctx.store
            .set_meta(meta::DONE_BASELINE_AT, timeutil::now_string())
            .await?;
    }
    Ok(())
}
