//! Bot API types the bridge reads and writes (subset).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<User>,
    pub chat: Chat,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub reply_to_message: Option<Box<ReplyRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReplyRef {
    pub message_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    #[serde(default)]
    pub message: Option<CallbackMessage>,
    #[serde(default)]
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallbackMessage {
    pub message_id: i64,
    pub chat: Chat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Button {
    pub text: String,
    pub callback_data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct InlineKeyboard {
    pub inline_keyboard: Vec<Vec<Button>>,
}

impl InlineKeyboard {
    pub fn is_empty(&self) -> bool {
        self.inline_keyboard.iter().all(Vec::is_empty)
    }

    pub fn row(&mut self, buttons: Vec<Button>) {
        if !buttons.is_empty() {
            self.inline_keyboard.push(buttons);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyMarkup {
    Inline(InlineKeyboard),
    /// `force_reply` prompt for free-text input.
    ForceReply,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotCommand {
    pub command: String,
    pub description: String,
}
