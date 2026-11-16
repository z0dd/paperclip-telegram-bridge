//! Outgoing Telegram queue: `retry_after` is honoured and pauses the queue,
//! messages keep their order (architecture §7.3).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bridge::core::sender::{self, SenderSettings, TgSender};
use bridge::health::HealthTracker;
use bridge::retry::Backoff;
use bridge::telegram::*;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Flaky {
    rate_limited_once: AtomicU32,
    transient_left: AtomicU32,
    sent: Mutex<Vec<(String, Instant)>>,
}

impl TelegramApi for Flaky {
    async fn get_updates(&self, _: i64, _: Duration) -> Result<Vec<Update>> {
        Ok(vec![])
    }
    async fn send_message(&self, _: i64, html: &str, _: Option<&ReplyMarkup>) -> Result<i64> {
        if self.rate_limited_once.swap(0, Ordering::SeqCst) == 1 {
            return Err(TelegramError::RateLimited {
                retry_after: Duration::from_secs(1),
            });
        }
        if self.transient_left.load(Ordering::SeqCst) > 0 {
            self.transient_left.fetch_sub(1, Ordering::SeqCst);
            return Err(TelegramError::Transient { reason: "connect" });
        }
        let mut s = self.sent.lock().unwrap();
        s.push((html.to_string(), Instant::now()));
        Ok(s.len() as i64)
    }
    async fn edit_message_text(
        &self,
        _: i64,
        _: i64,
        _: &str,
        _: Option<&InlineKeyboard>,
    ) -> Result<()> {
        Ok(())
    }
    async fn edit_reply_markup(&self, _: i64, _: i64, _: Option<&InlineKeyboard>) -> Result<()> {
        Ok(())
    }
    async fn answer_callback_query(&self, _: &str, _: Option<&str>) -> Result<()> {
        Ok(())
    }
    async fn set_my_commands(&self, _: i64, _: &[BotCommand]) -> Result<()> {
        Ok(())
    }
    async fn get_me(&self) -> Result<()> {
        Ok(())
    }
}

fn start(
    api: Arc<Flaky>,
    min_interval: Duration,
    max_attempts: u32,
) -> (TgSender, CancellationToken) {
    let (tx, rx) = TgSender::channel();
    let cancel = CancellationToken::new();
    tokio::spawn(sender::run(
        api,
        rx,
        SenderSettings {
            chat_id: 1,
            min_interval,
            backoff: Backoff::new(Duration::from_millis(1), Duration::from_millis(10)),
            max_attempts,
        },
        cancel.clone(),
        Arc::new(HealthTracker::new()),
    ));
    (tx, cancel)
}

#[tokio::test]
async fn retry_after_is_honoured_and_order_kept() {
    let api = Arc::new(Flaky::default());
    api.rate_limited_once.store(1, Ordering::SeqCst);
    let (tx, cancel) = start(api.clone(), Duration::ZERO, 3);
    let t0 = Instant::now();
    let a = tx.clone();
    let first = tokio::spawn(async move { a.send("first".into(), None).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = tx.send("second".into(), None).await.unwrap();
    first.await.unwrap().unwrap();
    assert_eq!(second, 2);
    let sent = api.sent.lock().unwrap().clone();
    assert_eq!(sent[0].0, "first");
    assert_eq!(sent[1].0, "second");
    assert!(
        sent[0].1 - t0 >= Duration::from_secs(1),
        "waited retry_after"
    );
    cancel.cancel();
}

#[tokio::test]
async fn messages_are_spaced_by_min_interval() {
    let api = Arc::new(Flaky::default());
    let (tx, cancel) = start(api.clone(), Duration::from_millis(300), 3);
    for n in 0..3 {
        tx.send(format!("m{n}"), None).await.unwrap();
    }
    let sent = api.sent.lock().unwrap().clone();
    assert!(sent[1].1 - sent[0].1 >= Duration::from_millis(290));
    assert!(sent[2].1 - sent[1].1 >= Duration::from_millis(290));
    cancel.cancel();
}

#[tokio::test]
async fn transient_errors_are_retried_up_to_the_limit() {
    let api = Arc::new(Flaky::default());
    api.transient_left.store(2, Ordering::SeqCst);
    let (tx, cancel) = start(api.clone(), Duration::ZERO, 3);
    assert_eq!(tx.send("ok".into(), None).await.unwrap(), 1);
    api.transient_left.store(5, Ordering::SeqCst);
    assert!(
        tx.send("fails".into(), None)
            .await
            .unwrap_err()
            .is_transient()
    );
    cancel.cancel();
    // After shutdown the sender refuses new work instead of hanging.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(tx.send("late".into(), None).await.is_err());
}
