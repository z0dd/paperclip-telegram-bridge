//! Heartbeat file and its freshness check (architecture §7.6). No network.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::timeutil;

/// The file must have been written within this window.
pub const FILE_MAX_AGE: Duration = Duration::from_secs(60);
pub const WRITE_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthState {
    pub pid: u32,
    pub started_at: String,
    pub written_at: String,
    pub last_telegram_ok_at: Option<String>,
    pub last_paperclip_ok_at: Option<String>,
    /// Last completed discovery (companies and agents) of all keys.
    #[serde(default)]
    pub last_discovery_ok_at: Option<String>,
    pub paperclip_auth_ok: bool,
    /// The supergroup passed the start checks (forum, bot admin with
    /// `can_manage_topics`).
    #[serde(default = "yes")]
    pub telegram_group_ok: bool,
    /// The bot may write to the group (no `403` on sending).
    #[serde(default = "yes")]
    pub telegram_send_ok: bool,
    /// Agent Chat is enabled in Paperclip.
    #[serde(default = "yes")]
    pub agent_chat_ok: bool,
    /// Pieces waiting for Telegram.
    #[serde(default)]
    pub queue_depth: u64,
}

fn yes() -> bool {
    true
}

/// Shared, lock-free record of the last successful calls and known problems.
#[derive(Debug)]
pub struct HealthTracker {
    started_at: i64,
    telegram_ok: AtomicI64,
    paperclip_ok: AtomicI64,
    discovery_ok: AtomicI64,
    auth_ok: AtomicBool,
    group_ok: AtomicBool,
    send_ok: AtomicBool,
    agent_chat_ok: AtomicBool,
    queue_depth: AtomicU64,
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn stamp(a: &AtomicI64) {
    a.store(timeutil::now().unix_timestamp(), Ordering::Relaxed);
}

impl HealthTracker {
    pub fn new() -> Self {
        Self {
            started_at: timeutil::now().unix_timestamp(),
            telegram_ok: AtomicI64::new(0),
            paperclip_ok: AtomicI64::new(0),
            discovery_ok: AtomicI64::new(0),
            auth_ok: AtomicBool::new(true),
            group_ok: AtomicBool::new(true),
            send_ok: AtomicBool::new(true),
            agent_chat_ok: AtomicBool::new(true),
            queue_depth: AtomicU64::new(0),
        }
    }

    pub fn telegram_ok(&self) {
        stamp(&self.telegram_ok);
    }

    pub fn paperclip_ok(&self) {
        stamp(&self.paperclip_ok);
    }

    pub fn discovery_ok(&self) {
        stamp(&self.discovery_ok);
    }

    pub fn set_paperclip_auth_ok(&self, ok: bool) {
        self.auth_ok.store(ok, Ordering::Relaxed);
    }

    pub fn is_paperclip_auth_ok(&self) -> bool {
        self.auth_ok.load(Ordering::Relaxed)
    }

    pub fn set_telegram_group_ok(&self, ok: bool) {
        self.group_ok.store(ok, Ordering::Relaxed);
    }

    pub fn set_telegram_send_ok(&self, ok: bool) {
        self.send_ok.store(ok, Ordering::Relaxed);
    }

    pub fn is_telegram_send_ok(&self) -> bool {
        self.send_ok.load(Ordering::Relaxed)
    }

    pub fn set_agent_chat_ok(&self, ok: bool) {
        self.agent_chat_ok.store(ok, Ordering::Relaxed);
    }

    pub fn set_queue_depth(&self, n: u64) {
        self.queue_depth.store(n, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> HealthState {
        let ts = |v: i64| {
            (v > 0)
                .then(|| OffsetDateTime::from_unix_timestamp(v).ok())
                .flatten()
                .map(timeutil::format)
        };
        HealthState {
            pid: std::process::id(),
            started_at: ts(self.started_at).unwrap_or_default(),
            written_at: timeutil::now_string(),
            last_telegram_ok_at: ts(self.telegram_ok.load(Ordering::Relaxed)),
            last_paperclip_ok_at: ts(self.paperclip_ok.load(Ordering::Relaxed)),
            last_discovery_ok_at: ts(self.discovery_ok.load(Ordering::Relaxed)),
            paperclip_auth_ok: self.auth_ok.load(Ordering::Relaxed),
            telegram_group_ok: self.group_ok.load(Ordering::Relaxed),
            telegram_send_ok: self.send_ok.load(Ordering::Relaxed),
            agent_chat_ok: self.agent_chat_ok.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
        }
    }
}

/// Atomic write: temp file in the same directory, fsync, rename.
pub fn write_file(path: &Path, state: &HealthState) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec(state)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Returns `Ok(())` when healthy, otherwise a reason without secrets.
pub fn check(path: &Path, max_stale: Duration, now: OffsetDateTime) -> Result<(), String> {
    let raw = std::fs::read(path).map_err(|e| format!("health file unreadable ({})", e.kind()))?;
    let state: HealthState =
        serde_json::from_slice(&raw).map_err(|_| "health file is corrupt".to_string())?;
    let age = |s: &str| timeutil::parse(s).map(|t| now - t);
    let written = age(&state.written_at).ok_or("health file has no timestamp")?;
    if written > FILE_MAX_AGE {
        return Err(format!(
            "health file is stale ({}s old)",
            written.whole_seconds()
        ));
    }
    if !state.paperclip_auth_ok {
        return Err("Paperclip key rejected (401)".into());
    }
    if !state.agent_chat_ok {
        return Err("Agent Chat is disabled in Paperclip".into());
    }
    if !state.telegram_group_ok {
        return Err(
            "Telegram supergroup check failed (forum, bot admin with can_manage_topics)".into(),
        );
    }
    if !state.telegram_send_ok {
        return Err("Telegram refuses messages to the supergroup (403)".into());
    }
    // Before the first success, count from process start.
    let started = age(&state.started_at).ok_or("health file has no start time")?;
    for (name, last) in [
        ("telegram", &state.last_telegram_ok_at),
        ("paperclip", &state.last_paperclip_ok_at),
    ] {
        let since = last.as_deref().and_then(age).unwrap_or(started);
        if since > max_stale {
            return Err(format!(
                "no successful {name} call for {}s",
                since.whole_seconds()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(now: OffsetDateTime, written: i64, tg: Option<i64>, pc: Option<i64>) -> HealthState {
        let at = |s: i64| timeutil::format(now - time::Duration::seconds(s));
        HealthState {
            pid: 1,
            started_at: at(3600),
            written_at: at(written),
            last_telegram_ok_at: tg.map(at),
            last_paperclip_ok_at: pc.map(at),
            last_discovery_ok_at: None,
            paperclip_auth_ok: true,
            telegram_group_ok: true,
            telegram_send_ok: true,
            agent_chat_ok: true,
            queue_depth: 0,
        }
    }

    #[test]
    fn fresh_file_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("health.json");
        let now = timeutil::now();
        write_file(&p, &state(now, 5, Some(10), Some(20))).unwrap();
        assert_eq!(check(&p, Duration::from_secs(300), now), Ok(()));
        assert!(!dir.path().join("health.tmp").exists());
    }

    #[test]
    fn unhealthy_cases() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("health.json");
        let now = timeutil::now();
        let max = Duration::from_secs(300);
        assert!(check(&p, max, now).unwrap_err().contains("unreadable"));

        write_file(&p, &state(now, 120, Some(1), Some(1))).unwrap();
        assert!(check(&p, max, now).unwrap_err().contains("stale"));

        write_file(&p, &state(now, 1, Some(600), Some(1))).unwrap();
        assert!(check(&p, max, now).unwrap_err().contains("telegram"));

        write_file(&p, &state(now, 1, Some(1), None)).unwrap();
        assert!(check(&p, max, now).unwrap_err().contains("paperclip"));

        let mut s = state(now, 1, Some(1), Some(1));
        s.paperclip_auth_ok = false;
        write_file(&p, &s).unwrap();
        assert!(check(&p, max, now).unwrap_err().contains("rejected"));

        for (flag, reason) in [(0, "Agent Chat"), (1, "supergroup"), (2, "403")] {
            let mut s = state(now, 1, Some(1), Some(1));
            match flag {
                0 => s.agent_chat_ok = false,
                1 => s.telegram_group_ok = false,
                _ => s.telegram_send_ok = false,
            }
            write_file(&p, &s).unwrap();
            assert!(
                check(&p, max, now).unwrap_err().contains(reason),
                "{reason}"
            );
        }

        std::fs::write(&p, b"{not json").unwrap();
        assert!(check(&p, max, now).unwrap_err().contains("corrupt"));
    }

    #[test]
    fn grace_after_start_before_first_success() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("health.json");
        let now = timeutil::now();
        let mut s = state(now, 1, None, None);
        s.started_at = timeutil::format(now - time::Duration::seconds(30));
        write_file(&p, &s).unwrap();
        assert_eq!(check(&p, Duration::from_secs(300), now), Ok(()));
    }

    #[test]
    fn tracker_snapshot() {
        let t = HealthTracker::new();
        assert!(t.snapshot().last_telegram_ok_at.is_none());
        t.telegram_ok();
        t.set_paperclip_auth_ok(false);
        t.set_queue_depth(7);
        t.discovery_ok();
        let s = t.snapshot();
        assert!(s.last_telegram_ok_at.is_some());
        assert!(s.last_discovery_ok_at.is_some());
        assert!(!s.paperclip_auth_ok);
        assert_eq!(s.queue_depth, 7);
        t.set_paperclip_auth_ok(true);
        assert!(t.snapshot().paperclip_auth_ok);
    }

    #[test]
    fn v1_health_file_is_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("health.json");
        let now = timeutil::now();
        let v1 = serde_json::json!({
            "pid": 1, "started_at": timeutil::format(now), "written_at": timeutil::format(now),
            "last_telegram_ok_at": null, "last_paperclip_ok_at": null, "paperclip_auth_ok": true
        });
        std::fs::write(&p, v1.to_string()).unwrap();
        assert_eq!(check(&p, Duration::from_secs(300), now), Ok(()));
    }
}
