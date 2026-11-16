//! Heartbeat file and its freshness check (architecture §7.6). No network.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
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
    pub paperclip_auth_ok: bool,
}

/// Shared, lock-free record of the last successful calls.
#[derive(Debug)]
pub struct HealthTracker {
    started_at: i64,
    telegram_ok: AtomicI64,
    paperclip_ok: AtomicI64,
    auth_ok: AtomicBool,
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthTracker {
    pub fn new() -> Self {
        Self {
            started_at: timeutil::now().unix_timestamp(),
            telegram_ok: AtomicI64::new(0),
            paperclip_ok: AtomicI64::new(0),
            auth_ok: AtomicBool::new(true),
        }
    }

    pub fn telegram_ok(&self) {
        self.telegram_ok
            .store(timeutil::now().unix_timestamp(), Ordering::Relaxed);
    }

    pub fn paperclip_ok(&self) {
        self.paperclip_ok
            .store(timeutil::now().unix_timestamp(), Ordering::Relaxed);
        self.auth_ok.store(true, Ordering::Relaxed);
    }

    pub fn paperclip_auth_failed(&self) {
        self.auth_ok.store(false, Ordering::Relaxed);
    }

    pub fn is_paperclip_auth_ok(&self) -> bool {
        self.auth_ok.load(Ordering::Relaxed)
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
            paperclip_auth_ok: self.auth_ok.load(Ordering::Relaxed),
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
        return Err("Paperclip key rejected (401/403)".into());
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
            paperclip_auth_ok: true,
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
        t.paperclip_auth_failed();
        let s = t.snapshot();
        assert!(s.last_telegram_ok_at.is_some());
        assert!(!s.paperclip_auth_ok);
        t.paperclip_ok();
        assert!(t.snapshot().paperclip_auth_ok);
    }
}
