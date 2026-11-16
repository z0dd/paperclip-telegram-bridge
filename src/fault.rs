//! Crash-injection points for recovery tests (feature `fault-injection`).
//! Without the feature `hit` is always `false` and compiles away.

#[cfg(feature = "fault-injection")]
mod imp {
    use std::collections::HashSet;
    use std::sync::Mutex;

    static ARMED: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

    pub fn arm(point: &'static str) {
        ARMED
            .lock()
            .unwrap()
            .get_or_insert_with(HashSet::new)
            .insert(point);
    }

    pub fn hit(point: &'static str) -> bool {
        ARMED
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|s| s.remove(point))
    }
}

#[cfg(feature = "fault-injection")]
pub use imp::{arm, hit};

#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub fn hit(_point: &'static str) -> bool {
    false
}

/// After `sendMessage` succeeded, before the delivery is committed as `sent`.
pub const AFTER_SEND_BEFORE_COMMIT: &str = "after_send_before_commit";
/// After the Paperclip HTTP call succeeded, before the outbox is committed.
pub const AFTER_HTTP_BEFORE_OUTBOX_COMMIT: &str = "after_http_before_outbox_commit";
