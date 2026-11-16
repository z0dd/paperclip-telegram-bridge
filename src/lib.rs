//! paperclip-telegram-bridge: delivers pending Paperclip interactions to one
//! Telegram user and sends the answers back.

pub mod config;
pub mod domain;
pub mod health;
pub mod logging;
pub mod paperclip;
pub mod retry;
pub mod shutdown;
pub mod timeutil;
