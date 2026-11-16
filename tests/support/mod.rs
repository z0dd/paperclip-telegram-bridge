//! Shared helpers for integration tests.
#![allow(dead_code)]

pub mod proxy;

/// Telegram half of the env-proxy test; filled in with the Telegram client.
pub async fn env_proxy_telegram_checks(_trap: &proxy::CountingListener) {}
