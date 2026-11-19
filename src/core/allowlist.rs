//! Update filter by chat and user (architecture §9, ADR-0010 §2.5). Foreign updates are
//! dropped silently; logging is rate-limited and carries only a short hash.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::logging::hash_id;
use crate::telegram::{Message, Update};

pub const LOG_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    ChatMismatch,
    UserMismatch,
    Unsupported,
}

impl Rejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatMismatch => "chat_mismatch",
            Self::UserMismatch => "user_mismatch",
            Self::Unsupported => "unsupported_update",
        }
    }
}

/// The forum supergroup and the Telegram users allowed to write from it
/// (ADR-0010 §2.5).
pub struct Allowlist {
    chat_id: i64,
    user_ids: Vec<i64>,
    log: Mutex<LogLimiter>,
}

struct LogLimiter {
    last: Option<Instant>,
    suppressed: u64,
}

impl Allowlist {
    pub fn new(chat_id: i64, user_ids: Vec<i64>) -> Self {
        Self {
            chat_id,
            user_ids,
            log: Mutex::new(LogLimiter {
                last: None,
                suppressed: 0,
            }),
        }
    }

    /// Chat first, then sender: a foreign chat is dropped whoever writes.
    pub fn check<'a>(&self, update: &'a Update) -> Result<&'a Message, Rejection> {
        let Some(m) = &update.message else {
            return Err(Rejection::Unsupported);
        };
        if m.chat.id != self.chat_id {
            return Err(Rejection::ChatMismatch);
        }
        match m.from.as_ref() {
            Some(u) if self.user_ids.contains(&u.id) => Ok(m),
            _ => Err(Rejection::UserMismatch),
        }
    }

    /// Logs a foreign update at most once per [`LOG_INTERVAL`], with a count
    /// of suppressed ones. Returns whether a record was written.
    pub fn log_rejection(&self, update: &Update, reason: Rejection) -> bool {
        self.log_rejection_at(update, reason, Instant::now())
    }

    fn log_rejection_at(&self, update: &Update, reason: Rejection, now: Instant) -> bool {
        let Ok(mut l) = self.log.lock() else {
            return false;
        };
        if l.last.is_some_and(|t| now.duration_since(t) < LOG_INTERVAL) {
            l.suppressed += 1;
            return false;
        }
        let kind = if update.message.is_some() {
            "message"
        } else {
            "other"
        };
        let sender = update
            .message
            .as_ref()
            .and_then(|m| m.from.as_ref())
            .map(|u| hash_id(u.id));
        if reason != Rejection::Unsupported {
            tracing::warn!(
                update_id = update.update_id,
                update_kind = kind,
                reason = reason.as_str(),
                sender_hash = sender.as_deref().unwrap_or("-"),
                suppressed_since_last = l.suppressed,
                "ignored update from outside the allowlist"
            );
        }
        l.last = Some(now);
        l.suppressed = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CHAT: i64 = -1001234567890;
    const USER: i64 = 111;
    const OTHER_ALLOWED: i64 = 222;

    fn update(v: serde_json::Value) -> Update {
        serde_json::from_value(v).unwrap()
    }

    fn msg(chat: i64, from: i64) -> Update {
        update(json!({"update_id": 1, "message": {
            "message_id": 5, "from": {"id": from, "is_bot": false, "first_name": "x"},
            "chat": {"id": chat, "type": "supergroup", "is_forum": true}, "date": 0,
            "message_thread_id": 7, "is_topic_message": true, "text": "hi"}}))
    }

    fn allowlist() -> Allowlist {
        Allowlist::new(CHAT, vec![USER, OTHER_ALLOWED])
    }

    #[test]
    fn admits_listed_users_in_the_group() {
        let a = allowlist();
        assert!(a.check(&msg(CHAT, USER)).is_ok());
        assert!(a.check(&msg(CHAT, OTHER_ALLOWED)).is_ok());
    }

    #[test]
    fn rejects_foreign_chat_and_user() {
        let a = allowlist();
        assert_eq!(
            a.check(&msg(-100999, USER)).unwrap_err(),
            Rejection::ChatMismatch
        );
        assert_eq!(
            a.check(&msg(USER, USER)).unwrap_err(),
            Rejection::ChatMismatch
        );
        assert_eq!(
            a.check(&msg(CHAT, 999)).unwrap_err(),
            Rejection::UserMismatch
        );
        let anonymous = update(json!({"update_id": 2, "message": {
            "message_id": 6, "chat": {"id": CHAT, "type": "supergroup"}, "text": "x"}}));
        assert_eq!(a.check(&anonymous).unwrap_err(), Rejection::UserMismatch);
        let unknown = update(json!({"update_id": 3, "edited_message": {}}));
        assert_eq!(a.check(&unknown).unwrap_err(), Rejection::Unsupported);
    }

    #[test]
    fn rejection_log_is_rate_limited() {
        let a = allowlist();
        let u = msg(999, 999);
        let t0 = Instant::now();
        assert!(a.log_rejection_at(&u, Rejection::ChatMismatch, t0));
        assert!(!a.log_rejection_at(&u, Rejection::ChatMismatch, t0 + Duration::from_secs(1)));
        assert!(!a.log_rejection_at(&u, Rejection::ChatMismatch, t0 + Duration::from_secs(9)));
        assert!(a.log_rejection_at(&u, Rejection::ChatMismatch, t0 + Duration::from_secs(11)));
    }

    #[test]
    fn topic_messages_are_parsed() {
        let m = msg(CHAT, USER).message.unwrap();
        assert_eq!(m.topic_thread_id(), Some(7));
        assert!(!m.is_topic_service());
        let general = update(json!({"update_id": 4, "message": {
            "message_id": 6, "from": {"id": USER}, "chat": {"id": CHAT, "type": "supergroup"},
            "text": "hi"}}));
        assert_eq!(general.message.unwrap().topic_thread_id(), None);
        let service = update(json!({"update_id": 5, "message": {
            "message_id": 7, "from": {"id": USER}, "chat": {"id": CHAT, "type": "supergroup"},
            "message_thread_id": 7, "is_topic_message": true,
            "forum_topic_closed": {}}}));
        assert!(service.message.unwrap().is_topic_service());
        let photo = update(json!({"update_id": 6, "message": {
            "message_id": 8, "from": {"id": USER}, "chat": {"id": CHAT, "type": "supergroup"},
            "photo": [{"file_id": "x"}]}}));
        assert!(photo.message.unwrap().text.is_none());
    }
}
