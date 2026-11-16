//! `/status`, `/backlog`, `/help` (architecture §8.4).

use super::dialog::Effects;
use super::{Ctx, Result};
use crate::domain::IssueSummary;
use crate::paperclip::PaperclipApi;
use crate::render::{escape, truncate_chars};
use crate::store::DeliveryStatus;
use crate::telegram::{BotCommand, TelegramApi};
use crate::timeutil;

pub const HELP: &str = "Я пересылаю вопросы и запросы подтверждения от агентов Paperclip и отправляю ваши ответы обратно.

Как отвечать:
• нажмите кнопку под вопросом;
• для текстового ответа нажмите «✍️ Ответить текстом» или ответьте (reply) на сообщение;
• формы из нескольких вопросов заполняются по шагам, ответ уходит после «Отправить»;
• запросы на действия инструментов (🛠) и доступ к секретам (🔑) можно разрешить или отклонить; необратимые действия требуют второго подтверждения.

Команды:
/status — что ждёт ответа и что сейчас в работе
/backlog — ближайшие задачи
/cancel — отменить ввод текста
/help — эта справка

Если вопрос уже решён в Paperclip, сообщение обновится само.";

pub const BACKLOG_TOP: usize = 10;

pub fn bot_commands() -> Vec<BotCommand> {
    [
        ("status", "Что ждёт ответа и что в работе"),
        ("backlog", "Ближайшие задачи"),
        ("cancel", "Отменить ввод текста"),
        ("help", "Справка"),
    ]
    .into_iter()
    .map(|(c, d)| BotCommand {
        command: c.into(),
        description: d.into(),
    })
    .collect()
}

pub async fn handle<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    name: &str,
) -> Result<Effects> {
    Ok(match name {
        "start" | "help" => Effects::message(escape(HELP)),
        "status" => Effects::message(status(ctx).await?),
        "backlog" => Effects::message(backlog(ctx).await),
        _ => Effects::message("Неизвестная команда. /help"),
    })
}

fn join_refs(refs: &[String]) -> String {
    let shown: Vec<String> = refs.iter().take(10).map(|r| escape(r)).collect();
    let mut s = shown.join(", ");
    if refs.len() > 10 {
        s.push_str(&format!(" и ещё {}", refs.len() - 10));
    }
    s
}

pub async fn status<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) -> Result<String> {
    let open = ctx
        .store
        .deliveries_with_status(vec![
            DeliveryStatus::Sent,
            DeliveryStatus::Answering,
            DeliveryStatus::Unsupported,
        ])
        .await?;
    let mut refs: Vec<String> = open.iter().map(|d| d.issue_ref.clone()).collect();
    refs.dedup();
    let queue = ctx.store.pending_outbox_count().await?;
    let mut lines = vec!["📊 <b>Статус Paperclip</b>".to_string()];
    lines.push(if open.is_empty() {
        "Ожидают ответа: 0".into()
    } else {
        format!("Ожидают ответа: {} ({})", open.len(), join_refs(&refs))
    });

    let mut available = ctx.paperclip_available();
    let (mut in_progress, mut in_review, mut blocked) = (0, 0, 0);
    let mut live = Vec::new();
    let mut failures = Vec::new();
    let day_ago = timeutil::now() - time::Duration::hours(24);
    if available {
        for company in &ctx.cfg.paperclip.company_ids {
            let issues = ctx
                .paperclip
                .issues(company, &["in_progress", "in_review", "blocked"], 1000)
                .await;
            let runs = ctx.paperclip.live_runs(company).await;
            let recent = ctx.paperclip.heartbeat_runs(company, 100).await;
            let (Ok(issues), Ok(runs), Ok(recent)) = (issues, runs, recent) else {
                available = false;
                break;
            };
            for i in &issues {
                ctx.remember_issue(&i.id, i.identifier.as_deref(), Some(&i.title));
                match i.status.as_str() {
                    "in_progress" => in_progress += 1,
                    "in_review" => in_review += 1,
                    "blocked" => blocked += 1,
                    _ => {}
                }
            }
            for r in runs.iter().filter(|r| r.is_live()) {
                let name = ctx
                    .agent_name(company, r.agent_id.as_deref())
                    .await
                    .unwrap_or_else(|| "агент".into());
                live.push(match r.issue_id.as_deref().and_then(|i| ctx.issue_ref(i)) {
                    Some(issue) => format!("{} ({})", escape(&name), escape(&issue)),
                    None => escape(&name),
                });
            }
            for r in recent.iter().filter(|r| r.is_failure()) {
                let at = r
                    .finished_at
                    .as_deref()
                    .or(r.created_at.as_deref())
                    .and_then(timeutil::parse);
                if at.is_some_and(|t| t >= day_ago) {
                    let name = ctx
                        .agent_name(company, r.agent_id.as_deref())
                        .await
                        .unwrap_or_else(|| "агент".into());
                    failures.push(match r.issue_id.as_deref().and_then(|i| ctx.issue_ref(i)) {
                        Some(issue) => format!("{}, {}", escape(&name), escape(&issue)),
                        None => escape(&name),
                    });
                }
            }
        }
    }
    if available {
        lines.push(format!(
            "В работе: {in_progress} · На ревью: {in_review} · Заблокировано: {blocked}"
        ));
        lines.push(if live.is_empty() {
            "Сейчас работают агенты: нет".into()
        } else {
            format!("Сейчас работают агенты: {}", live.join(", "))
        });
        lines.push(if failures.is_empty() {
            "Сбои за 24 ч: 0".into()
        } else {
            let mut shown = failures.clone();
            shown.truncate(5);
            format!("Сбои за 24 ч: {} ({})", failures.len(), shown.join("; "))
        });
        lines.push(format!("Bridge: Paperclip ✅ · очередь ответов: {queue}"));
    } else {
        lines.push("Paperclip недоступен".into());
        lines.push(format!("Bridge: Paperclip ❌ · очередь ответов: {queue}"));
    }
    Ok(lines.join("\n"))
}

pub fn priority_icon(i: &IssueSummary) -> &'static str {
    match i.priority_rank() {
        0 => "🔴",
        1 => "🟠",
        2 => "🟡",
        3 => "⚪",
        _ => "▫️",
    }
}

pub async fn backlog<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) -> String {
    if !ctx.paperclip_available() {
        return "📋 Бэклог\nPaperclip недоступен".into();
    }
    let mut all = Vec::new();
    for company in &ctx.cfg.paperclip.company_ids {
        match ctx
            .paperclip
            .issues(company, &["todo", "backlog"], 20)
            .await
        {
            Ok(list) => all.extend(list),
            Err(e) => {
                tracing::warn!(class = %e.class(), "backlog unavailable");
                return "📋 Бэклог\nPaperclip недоступен".into();
            }
        }
    }
    if all.is_empty() {
        return "📋 Бэклог пуст".into();
    }
    // Stable: server order (activity) within the same priority.
    all.sort_by_key(IssueSummary::priority_rank);
    let mut lines = vec![format!(
        "📋 <b>Бэклог (топ {BACKLOG_TOP} по приоритету)</b>"
    )];
    for i in all.iter().take(BACKLOG_TOP) {
        lines.push(format!(
            "{} {} {} — {}",
            priority_icon(i),
            escape(i.identifier.as_deref().unwrap_or("—")),
            escape(&truncate_chars(&i.title, 60)),
            escape(&i.status)
        ));
    }
    if all.len() > BACKLOG_TOP {
        lines.push(format!("…и ещё {}", all.len() - BACKLOG_TOP));
    }
    lines.join("\n")
}
