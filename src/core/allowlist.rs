//! Update filter by chat and user (architecture §9). Foreign updates are
//! dropped silently; logging is rate-limited and carries only a short hash.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::logging::hash_id;
use crate::telegram::{CallbackQuery, Message, Update};

pub const LOG_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admitted<'a> {
    Message(&'a Message),
    Callback(&'a CallbackQuery),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    ChatMismatch,
    UserMismatch,
    NotPrivate,
    Unsupported,
}

impl Rejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatMismatch => "chat_mismatch",
            Self::UserMismatch => "user_mismatch",
            Self::NotPrivate => "not_private",
            Self::Unsupported => "unsupported_update",
        }
    }
}

pub struct Allowlist {
    chat_id: i64,
    user_id: i64,
    log: Mutex<LogLimiter>,
}

struct LogLimiter {
    last: Option<Instant>,
    suppressed: u64,
}

impl Allowlist {
    pub fn new(chat_id: i64, user_id: i64) -> Self {
        Self {
            chat_id,
            user_id,
            log: Mutex::new(LogLimiter {
                last: None,
                suppressed: 0,
            }),
        }
    }

    pub fn check<'a>(&self, update: &'a Update) -> Result<Admitted<'a>, Rejection> {
        if let Some(m) = &update.message {
            let from = m.from.as_ref().map(|u| u.id);
            if m.chat.id != self.chat_id {
                return Err(Rejection::ChatMismatch);
            }
            if from != Some(self.user_id) {
                return Err(Rejection::UserMismatch);
            }
            if m.chat.kind != "private" {
                return Err(Rejection::NotPrivate);
            }
            return Ok(Admitted::Message(m));
        }
        if let Some(cq) = &update.callback_query {
            if cq.from.id != self.user_id {
                return Err(Rejection::UserMismatch);
            }
            if cq.message.as_ref().map(|m| m.chat.id) != Some(self.chat_id) {
                return Err(Rejection::ChatMismatch);
            }
            return Ok(Admitted::Callback(cq));
        }
        Err(Rejection::Unsupported)
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
        } else if update.callback_query.is_some() {
            "callback_query"
        } else {
            "other"
        };
        let sender = update
            .message
            .as_ref()
            .and_then(|m| m.from.as_ref())
            .or(update.callback_query.as_ref().map(|c| &c.from))
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

    const CHAT: i64 = 111;
    const USER: i64 = 111;

    fn update(v: serde_json::Value) -> Update {
        serde_json::from_value(v).unwrap()
    }

    fn msg(chat: i64, chat_type: &str, from: i64) -> Update {
        update(json!({"update_id": 1, "message": {
            "message_id": 5, "from": {"id": from, "is_bot": false, "first_name": "x"},
            "chat": {"id": chat, "type": chat_type}, "date": 0, "text": "hi"}}))
    }

    fn cb(chat: i64, from: i64) -> Update {
        update(json!({"update_id": 2, "callback_query": {
            "id": "q", "from": {"id": from, "is_bot": false, "first_name": "x"},
            "message": {"message_id": 7, "chat": {"id": chat, "type": "private"}, "date": 0},
            "chat_instance": "ci", "data": "v1:ac:1:0"}}))
    }

    #[test]
    fn admits_owner() {
        let a = Allowlist::new(CHAT, USER);
        assert!(matches!(
            a.check(&msg(CHAT, "private", USER)),
            Ok(Admitted::Message(_))
        ));
        assert!(matches!(
            a.check(&cb(CHAT, USER)),
            Ok(Admitted::Callback(_))
        ));
    }

    #[test]
    fn rejects_foreign_chat_user_and_groups() {
        let a = Allowlist::new(CHAT, USER);
        assert_eq!(
            a.check(&msg(999, "private", USER)),
            Err(Rejection::ChatMismatch)
        );
        assert_eq!(
            a.check(&msg(CHAT, "private", 999)),
            Err(Rejection::UserMismatch)
        );
        assert_eq!(
            a.check(&msg(CHAT, "group", USER)),
            Err(Rejection::NotPrivate)
        );
        assert_eq!(a.check(&cb(CHAT, 999)), Err(Rejection::UserMismatch));
        assert_eq!(a.check(&cb(999, USER)), Err(Rejection::ChatMismatch));
        // Group with the owner as sender but another chat id.
        assert_eq!(
            a.check(&msg(-100123, "supergroup", USER)),
            Err(Rejection::ChatMismatch)
        );
        let unknown = update(json!({"update_id": 3, "edited_message": {}}));
        assert_eq!(a.check(&unknown), Err(Rejection::Unsupported));
    }

    #[test]
    fn rejection_log_is_rate_limited() {
        let a = Allowlist::new(CHAT, USER);
        let u = msg(999, "private", 999);
        let t0 = Instant::now();
        assert!(a.log_rejection_at(&u, Rejection::ChatMismatch, t0));
        assert!(!a.log_rejection_at(&u, Rejection::ChatMismatch, t0 + Duration::from_secs(1)));
        assert!(!a.log_rejection_at(&u, Rejection::ChatMismatch, t0 + Duration::from_secs(9)));
        assert!(a.log_rejection_at(&u, Rejection::ChatMismatch, t0 + Duration::from_secs(11)));
    }

    #[test]
    fn updates_are_parsed() {
        let m = msg(CHAT, "private", USER);
        let mm = m.message.unwrap();
        assert_eq!(mm.text.as_deref(), Some("hi"));
        assert!(mm.reply_to_message.is_none());
        let r = update(json!({"update_id": 4, "message": {
            "message_id": 6, "from": {"id": USER}, "chat": {"id": CHAT, "type": "private"},
            "text": "answer", "reply_to_message": {"message_id": 5, "chat": {"id": CHAT, "type": "private"}}}}));
        assert_eq!(r.message.unwrap().reply_to_message.unwrap().message_id, 5);
        let photo = update(json!({"update_id": 5, "message": {
            "message_id": 7, "from": {"id": USER}, "chat": {"id": CHAT, "type": "private"},
            "photo": [{"file_id": "x"}]}}));
        assert!(photo.message.unwrap().text.is_none());
        let c = cb(CHAT, USER).callback_query.unwrap();
        assert_eq!(c.data.as_deref(), Some("v1:ac:1:0"));
    }
}
