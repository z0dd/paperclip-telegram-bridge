//! The single Telegram queue (ADR-0010 §2.2): calls are spaced by
//! `TELEGRAM_GROUP_MIN_INTERVAL`, `retry_after` pauses the whole queue and
//! the order is kept.

mod support;

use std::time::{Duration, Instant};

use bridge::core::sender::Sender;
use bridge::core::{Bridge, Ctx};
use bridge::health::HealthTracker;
use bridge::paperclip::HttpPaperclipClient;
use bridge::store::Store;
use bridge::telegram::TelegramError;
use std::sync::Arc;
use support::harness::{FakeTelegram, Harness};
use support::paperclip::PaperclipMock;
use tokio_util::sync::CancellationToken;

/// A context whose sender runs as the real task.
async fn running(
    interval: &str,
) -> (
    Harness,
    Ctx<HttpPaperclipClient, FakeTelegram>,
    Arc<FakeTelegram>,
    CancellationToken,
) {
    let pc = PaperclipMock::start().await;
    let h = Harness::start(&pc, &[("TELEGRAM_GROUP_MIN_INTERVAL", interval)]).await;
    let cfg = bridge::config::Config::from_env(&bridge::config::Env::new(&h.env)).unwrap();
    let store = Store::open(&cfg.state_path).unwrap();
    let keys = HttpPaperclipClient::from_config(&cfg.paperclip)
        .unwrap()
        .into_iter()
        .map(Arc::new)
        .collect();
    let tg = Arc::new(FakeTelegram::default());
    let (ctx, notices) = Bridge::new(
        Arc::new(cfg),
        keys,
        tg.clone(),
        store,
        Arc::new(HealthTracker::new()),
    );
    let cancel = CancellationToken::new();
    tokio::spawn(Sender::new(ctx.clone(), notices).run(cancel.clone()));
    (h, ctx, tg, cancel)
}

async fn wait_sent(tg: &FakeTelegram, n: usize, limit: Duration) {
    let t0 = Instant::now();
    while tg.sent().len() < n {
        assert!(t0.elapsed() < limit, "only {} of {n} sent", tg.sent().len());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_are_spaced_by_the_group_interval() {
    let (_h, ctx, tg, cancel) = running("300ms").await;
    let t0 = Instant::now();
    for i in 0..3 {
        ctx.notify(None, format!("n{i}"));
    }
    wait_sent(&tg, 3, Duration::from_secs(5)).await;
    assert!(
        t0.elapsed() >= Duration::from_millis(600),
        "{:?}",
        t0.elapsed()
    );
    let texts: Vec<String> = tg.sent().into_iter().map(|s| s.text).collect();
    assert_eq!(texts, ["n0", "n1", "n2"]);
    cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_after_pauses_the_queue() {
    let (_h, ctx, tg, cancel) = running("1ms").await;
    // The first call is rate limited: nothing goes out until retry_after.
    tg.st().send_errors.push_back(TelegramError::RateLimited {
        retry_after: Duration::from_secs(1),
    });
    let t0 = Instant::now();
    ctx.notify(None, "first");
    ctx.notify(None, "second");
    wait_sent(&tg, 2, Duration::from_secs(5)).await;
    assert!(t0.elapsed() >= Duration::from_secs(1), "{:?}", t0.elapsed());
    let texts: Vec<String> = tg.sent().into_iter().map(|s| s.text).collect();
    assert_eq!(texts, ["first", "second"], "order kept after the pause");
    cancel.cancel();
}
