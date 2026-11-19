//! Bot API types the bridge reads (subset).

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<User>,
    pub chat: Chat,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub is_topic_message: Option<bool>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub forum_topic_created: Option<Value>,
    #[serde(default)]
    pub forum_topic_edited: Option<Value>,
    #[serde(default)]
    pub forum_topic_closed: Option<Value>,
    #[serde(default)]
    pub forum_topic_reopened: Option<Value>,
    #[serde(default)]
    pub general_forum_topic_hidden: Option<Value>,
    #[serde(default)]
    pub general_forum_topic_unhidden: Option<Value>,
}

impl Message {
    /// Service message about topics (created, renamed, closed, …).
    pub fn is_topic_service(&self) -> bool {
        self.forum_topic_created.is_some()
            || self.forum_topic_edited.is_some()
            || self.forum_topic_closed.is_some()
            || self.forum_topic_reopened.is_some()
            || self.general_forum_topic_hidden.is_some()
            || self.general_forum_topic_unhidden.is_some()
    }

    /// The topic the message was written in; `None` for General.
    pub fn topic_thread_id(&self) -> Option<i64> {
        if self.is_topic_message == Some(true) {
            self.message_thread_id
        } else {
            None
        }
    }
}

/// `getChat` (subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatInfo {
    pub kind: String,
    pub is_forum: bool,
}

/// `getChatMember` (subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    pub status: String,
    pub can_manage_topics: bool,
}
