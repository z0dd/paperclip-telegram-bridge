//! Run failure notifications, completed tasks and commands (stage 7).

mod support;

use std::sync::{Arc, Mutex};

use bridge::core::runs;
use serde_json::{Value, json};
use support::harness::Harness;
use support::paperclip::*;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ago(minutes: i64) -> String {
    bridge::timeutil::format(bridge::timeutil::now() - time::Duration::minutes(minutes))
}

fn later(minutes: i64) -> String {
    bridge::timeutil::format(bridge::timeutil::now() + time::Duration::minutes(minutes))
}

fn run(id: &str, status: &str, finished: &str) -> Value {
    json!({"id": id, "agentId": "agent-1", "issueId": ISSUE, "status": status,
           "error": "SECRET-ERROR-TEXT /home/user/.ssh", "stderrExcerpt": "SECRET-STDERR",
           "errorCode": "adapter_failed", "createdAt": finished, "finishedAt": finished,
           "scheduledRetryAt": null})
}

struct Runs(Arc<Mutex<Vec<Value>>>);

impl Runs {
    async fn mount(server: &MockServer, initial: Vec<Value>) -> Self {
        let list = Arc::new(Mutex::new(initial));
        let l = list.clone();
        Mock::given(method("GET"))
            .and(path(format!("/api/companies/{COMPANY}/heartbeat-runs")))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(Value::Array(l.lock().unwrap().clone()))
            })
            .mount(server)
            .await;
        Self(list)
    }

    fn push(&self, r: Value) {
        self.0.lock().unwrap().insert(0, r);
    }
}

async fn mount_issues(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/issues")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": ISSUE, "identifier": "PET-1", "title": "Issue title", "status": "in_progress", "priority": "high"}
        ])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn only_new_failures_are_reported_once() {
    let server = MockServer::start().await;
    serve(&server, &[], vec![]).await;
    mount_issues(&server).await;
    let runs_list = Runs::mount(&server, vec![run("run-old", "failed", &ago(60))]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    runs::cycle(&h.ctx).await.unwrap();
    assert!(
        h.tg.texts().is_empty(),
        "baseline: existing failures are not reported"
    );

    runs_list.push(run("run-new", "timed_out", &later(1)));
    runs_list.push(run("run-cancel", "cancelled", &later(1)));
    runs_list.push(run("run-int", "interrupted", &later(1)));
    let mut retry = run("run-retry", "failed", &later(1));
    retry["scheduledRetryAt"] = json!(later(5));
    runs_list.push(retry);
    runs_list.push(run("run-ok", "succeeded", &later(1)));
    runs::cycle(&h.ctx).await.unwrap();
    let texts = h.tg.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    let t = &texts[0];
    assert!(t.contains("⚠️ <b>Сбой агента Architect</b> · PET-1"), "{t}");
    assert!(t.contains("Статус: timed_out · код: adapter_failed"));
    assert!(t.contains("Время: "));
    assert!(!t.contains("SECRET"), "error text is never forwarded");

    runs::cycle(&h.ctx).await.unwrap();
    let h = h.restart().await;
    runs::cycle(&h.ctx).await.unwrap();
    assert_eq!(h.tg.texts().len(), 1, "no duplicates, also after a restart");
}

#[tokio::test]
async fn failed_run_from_the_feed_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/attention")))
        .respond_with(ResponseTemplate::new(200).set_body_json(attention(vec![json!({
            "id": "failed_run:run:run-9", "sourceKind": "failed_run", "activityAt": ago(1),
            "subject": {"kind": "run", "id": "run-9",
                        "metadata": {"agentId": "agent-1", "issueId": ISSUE, "errorCode": "retry_exhausted",
                                     "error": "SECRET-ERROR-TEXT"}},
            "relatedIssue": {"id": ISSUE, "identifier": "PET-1", "title": "t"}
        })])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/agents")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": "agent-1", "name": "Developer"}])),
        )
        .mount(&server)
        .await;
    let _runs = Runs::mount(&server, vec![]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    runs::cycle(&h.ctx).await.unwrap(); // baseline
    h.sync().await;
    runs::cycle(&h.ctx).await.unwrap();
    h.sync().await;
    runs::cycle(&h.ctx).await.unwrap();
    let texts = h.tg.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].contains("Сбой агента Developer</b> · PET-1"));
    assert!(texts[0].contains("код: retry_exhausted"));
    assert!(!texts[0].contains("SECRET"));
}

#[tokio::test]
async fn run_failure_notifications_can_be_disabled() {
    let server = MockServer::start().await;
    serve(&server, &[], vec![]).await;
    let runs_list = Runs::mount(&server, vec![]).await;
    let h = Harness::start(&server.uri(), &[("NOTIFY_RUN_FAILURES", "false")]).await;
    runs::cycle(&h.ctx).await.unwrap();
    runs_list.push(run("run-new", "failed", &later(1)));
    runs::cycle(&h.ctx).await.unwrap();
    assert!(h.tg.texts().is_empty());
}

#[tokio::test]
async fn completed_tasks_only_with_flag() {
    let server = MockServer::start().await;
    serve(&server, &[], vec![]).await;
    let _runs = Runs::mount(&server, vec![]).await;
    let done = Arc::new(Mutex::new(vec![
        json!({"id": "i-old", "identifier": "PET-1", "title": "Old", "status": "done"}),
    ]));
    let d = done.clone();
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/issues")))
        .and(query_param("status", "done"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(Value::Array(d.lock().unwrap().clone()))
        })
        .mount(&server)
        .await;

    let off = Harness::start(&server.uri(), &[]).await;
    runs::cycle(&off.ctx).await.unwrap();
    done.lock().unwrap().push(
        json!({"id": "i-new", "identifier": "PET-2", "title": "New <task>", "status": "done"}),
    );
    runs::cycle(&off.ctx).await.unwrap();
    assert!(off.tg.texts().is_empty());

    done.lock().unwrap().truncate(1);
    let on = Harness::start(&server.uri(), &[("NOTIFY_TASK_COMPLETED", "true")]).await;
    runs::cycle(&on.ctx).await.unwrap();
    assert!(on.tg.texts().is_empty(), "baseline");
    done.lock().unwrap().push(
        json!({"id": "i-new", "identifier": "PET-2", "title": "New <task>", "status": "done"}),
    );
    runs::cycle(&on.ctx).await.unwrap();
    runs::cycle(&on.ctx).await.unwrap();
    assert_eq!(
        on.tg.texts(),
        vec!["🏁 PET-2 завершена: New &lt;task&gt;".to_string()]
    );
}

#[tokio::test]
async fn status_and_backlog_commands() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/issues")))
        .and(query_param("status", "in_progress,in_review,blocked"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": ISSUE, "identifier": "PET-1", "title": "A", "status": "in_progress"},
            {"id": "i-2", "identifier": "PET-2", "title": "B", "status": "in_progress"},
            {"id": "i-3", "identifier": "PET-3", "title": "C", "status": "blocked"}
        ])))
        .mount(&server)
        .await;
    let backlog: Vec<Value> = (0..13)
        .map(|n| {
            json!({"id": format!("b-{n}"), "identifier": format!("PET-{}", 20 + n), "title": format!("Task <{n}>"),
                   "status": "todo", "priority": if n == 5 { "critical" } else if n % 2 == 0 { "low" } else { "medium" }})
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/issues")))
        .and(query_param("status", "todo,backlog"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Array(backlog)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/live-runs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "r-1", "agentId": "agent-1", "issueId": ISSUE, "status": "running"}])))
        .mount(&server)
        .await;
    let _runs = Runs::mount(
        &server,
        vec![
            run("r-f", "failed", &ago(30)),
            run("r-old", "failed", &ago(60 * 30)),
        ],
    )
    .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;

    h.text("/status", None).await;
    let status = h.tg.texts().last().unwrap().clone();
    assert!(status.contains("📊 <b>Статус Paperclip</b>"), "{status}");
    assert!(status.contains("Ожидают ответа: 1 (PET-1)"));
    assert!(status.contains("В работе: 2 · На ревью: 0 · Заблокировано: 1"));
    assert!(status.contains("Сейчас работают агенты: Architect (PET-1)"));
    assert!(status.contains("Сбои за 24 ч: 1 (Architect, PET-1)"));
    assert!(status.contains("Bridge: Paperclip ✅ · очередь ответов: 0"));

    h.text("/backlog@bridge_bot", None).await;
    let b = h.tg.texts().last().unwrap().clone();
    let lines: Vec<&str> = b.lines().collect();
    assert_eq!(lines[0], "📋 <b>Бэклог (топ 10 по приоритету)</b>");
    assert_eq!(lines[1], "🔴 PET-25 Task &lt;5&gt; — todo");
    assert!(lines[2].starts_with("🟡 "));
    assert_eq!(lines.len(), 12);
    assert_eq!(lines[11], "…и ещё 3");

    h.text("/help", None).await;
    assert!(
        h.tg.texts()
            .last()
            .unwrap()
            .contains("/status — что ждёт ответа")
    );
    h.text("/start", None).await;
    assert!(h.tg.texts().last().unwrap().contains("Я пересылаю вопросы"));
    h.text("/unknown", None).await;
    assert!(h.tg.texts().last().unwrap().contains("Неизвестная команда"));
}

#[tokio::test]
async fn commands_report_paperclip_unavailable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    for _ in 0..3 {
        assert!(h.try_sync().await.is_err());
    }
    let texts = h.tg.texts();
    assert_eq!(texts.len(), 1);
    assert!(
        texts[0].starts_with("⚠️ Paperclip недоступен с "),
        "{}",
        texts[0]
    );
    h.text("/status", None).await;
    let s = h.tg.texts().last().unwrap().clone();
    assert!(s.contains("Paperclip недоступен"), "{s}");
    assert!(s.contains("Paperclip ❌"));
    h.text("/backlog", None).await;
    assert!(
        h.tg.texts()
            .last()
            .unwrap()
            .contains("Paperclip недоступен")
    );

    server.reset().await;
    serve(&server, &[], vec![]).await;
    h.sync().await;
    assert!(
        h.tg.texts()
            .last()
            .unwrap()
            .contains("✅ Связь с Paperclip восстановлена")
    );
}

#[tokio::test]
async fn revoked_key_is_reported_once_and_makes_health_fail() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "Unauthorized"})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    assert!(h.try_sync().await.is_err());
    assert!(h.try_sync().await.is_err());
    let texts = h.tg.texts();
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("Ключ Paperclip отклонён"));
    assert!(!h.ctx.health.snapshot().paperclip_auth_ok);
}
