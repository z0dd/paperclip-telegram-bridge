//! paperclip-telegram-bridge: mirrors Paperclip Agent Chat conversations into
//! topics of one Telegram forum supergroup and sends the user's messages back.

pub mod app;
pub mod config;
pub mod core;
pub mod domain;
pub mod fault;
pub mod health;
pub mod logging;
pub mod paperclip;
pub mod render;
pub mod retry;
pub mod shutdown;
pub mod store;
pub mod telegram;
pub mod timeutil;
