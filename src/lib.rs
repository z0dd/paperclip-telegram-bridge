//! paperclip-telegram-bridge: delivers pending Paperclip interactions to one
//! Telegram user and sends the answers back.

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
