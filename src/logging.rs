//! `tracing` setup. Log call sites only pass ids, kinds, error classes and
//! durations; this module only selects level and format.

use std::sync::OnceLock;

use crate::config::{LogConfig, LogFormat};

pub fn init(cfg: &LogConfig) {
    let builder = tracing_subscriber::fmt()
        .with_max_level(cfg.level)
        .with_target(false)
        .with_writer(std::io::stderr);
    let _ = match cfg.format {
        LogFormat::Json => builder
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .try_init(),
        LogFormat::Text => builder.with_ansi(false).try_init(),
    };
}

/// Short, stable hash of a foreign Telegram id for log correlation
/// (`sha256(id)[..8]`, architecture §9).
pub fn hash_id(id: i64) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(id.to_string().as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// Installs the `ring` rustls provider once per process. reqwest is built
/// without a default provider so that aws-lc never enters the build.
pub fn install_tls_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_short_and_stable() {
        let a = hash_id(987654321);
        assert_eq!(a.len(), 8);
        assert_eq!(a, hash_id(987654321));
        assert_ne!(a, hash_id(987654322));
        assert!(!a.contains("987654321"));
    }
}
