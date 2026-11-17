//! Paperclip version check at startup (ADR-0009 §3). Informational only:
//! the start is never blocked.

/// The Paperclip release the bridge was tested against. The only place where
/// the number is written down.
pub const TESTED_PAPERCLIP_VERSION: &str = "2026.1106.0";

/// The server value is logged at most this long.
const MAX_LOGGED_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCheck<'a> {
    Tested,
    Other(&'a str),
    Missing,
    Unrecognized(&'a str),
}

/// `YYYY.MMDD.N`: three dot-separated ASCII-digit parts of 4, 4 and ≥ 1
/// characters. No calendar check.
fn is_release_format(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    matches!(parts.as_slice(), [y, md, n]
        if y.len() == 4 && md.len() == 4 && digits(y) && digits(md) && digits(n))
}

pub fn classify(version: Option<&str>) -> VersionCheck<'_> {
    let Some(v) = version.map(str::trim).filter(|v| !v.is_empty()) else {
        return VersionCheck::Missing;
    };
    if !is_release_format(v) {
        VersionCheck::Unrecognized(v)
    } else if v == TESTED_PAPERCLIP_VERSION {
        VersionCheck::Tested
    } else {
        VersionCheck::Other(v)
    }
}

fn shown(v: &str) -> String {
    v.chars().take(MAX_LOGGED_CHARS).collect()
}

/// Logs the outcome of [`classify`]: INFO for the tested version, WARN otherwise.
pub fn log_check(version: Option<&str>) {
    match classify(version) {
        VersionCheck::Tested => tracing::info!(
            paperclip_version = TESTED_PAPERCLIP_VERSION,
            version_status = "tested",
            "paperclip reachable"
        ),
        VersionCheck::Other(v) => tracing::warn!(
            paperclip_version = %shown(v),
            tested_version = TESTED_PAPERCLIP_VERSION,
            "paperclip version differs from the tested one"
        ),
        VersionCheck::Missing => tracing::warn!(
            tested_version = TESTED_PAPERCLIP_VERSION,
            "paperclip version is missing"
        ),
        VersionCheck::Unrecognized(v) => tracing::warn!(
            paperclip_version = %shown(v),
            tested_version = TESTED_PAPERCLIP_VERSION,
            "paperclip version is not recognized"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn classify_table() {
        let cases: &[(Option<&str>, VersionCheck<'_>)] = &[
            (Some("2026.1106.0"), VersionCheck::Tested),
            (Some(" 2026.1106.0 "), VersionCheck::Tested),
            (Some("2026.1106.1"), VersionCheck::Other("2026.1106.1")),
            (Some("2027.0101.12"), VersionCheck::Other("2027.0101.12")),
            (None, VersionCheck::Missing),
            (Some(""), VersionCheck::Missing),
            (Some("  "), VersionCheck::Missing),
            (
                Some("v2026.1106.0"),
                VersionCheck::Unrecognized("v2026.1106.0"),
            ),
            (Some("2026.11.6"), VersionCheck::Unrecognized("2026.11.6")),
            (Some("latest"), VersionCheck::Unrecognized("latest")),
            (Some("2026.1106."), VersionCheck::Unrecognized("2026.1106.")),
            (
                Some("2026.1106.0.1"),
                VersionCheck::Unrecognized("2026.1106.0.1"),
            ),
            (
                Some("2026.1106.-1"),
                VersionCheck::Unrecognized("2026.1106.-1"),
            ),
            (
                Some("２０２６.1106.0"),
                VersionCheck::Unrecognized("２０２６.1106.0"),
            ),
        ];
        for (input, want) in cases {
            assert_eq!(classify(*input), *want, "{input:?}");
        }
    }

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// JSON log lines written by `log_check(version)`.
    fn logged(version: Option<&str>) -> Vec<serde_json::Value> {
        let buf = Buf::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(buf.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || log_check(version));
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        out.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn levels(lines: &[serde_json::Value]) -> Vec<&str> {
        lines.iter().map(|l| l["level"].as_str().unwrap()).collect()
    }

    #[test]
    fn tested_version_is_one_info_without_warn() {
        let lines = logged(Some(TESTED_PAPERCLIP_VERSION));
        assert_eq!(levels(&lines), ["INFO"]);
        let f = &lines[0]["fields"];
        assert_eq!(f["message"], "paperclip reachable");
        assert_eq!(f["paperclip_version"], TESTED_PAPERCLIP_VERSION);
        assert_eq!(f["version_status"], "tested");
    }

    #[test]
    fn other_version_warns_with_both_versions() {
        let lines = logged(Some("2026.1201.3"));
        assert_eq!(levels(&lines), ["WARN"]);
        let f = &lines[0]["fields"];
        assert_eq!(
            f["message"],
            "paperclip version differs from the tested one"
        );
        assert_eq!(f["paperclip_version"], "2026.1201.3");
        assert_eq!(f["tested_version"], TESTED_PAPERCLIP_VERSION);
    }

    #[test]
    fn missing_version_warns() {
        for v in [None, Some(""), Some("   ")] {
            let lines = logged(v);
            assert_eq!(levels(&lines), ["WARN"], "{v:?}");
            let f = &lines[0]["fields"];
            assert_eq!(f["message"], "paperclip version is missing");
            assert_eq!(f["tested_version"], TESTED_PAPERCLIP_VERSION);
            assert!(f.get("paperclip_version").is_none());
        }
    }

    #[test]
    fn unrecognized_version_warns_and_is_truncated() {
        let lines = logged(Some("latest"));
        assert_eq!(levels(&lines), ["WARN"]);
        let f = &lines[0]["fields"];
        assert_eq!(f["message"], "paperclip version is not recognized");
        assert_eq!(f["paperclip_version"], "latest");
        assert_eq!(f["tested_version"], TESTED_PAPERCLIP_VERSION);

        let long = "x".repeat(500);
        let lines = logged(Some(&long));
        assert_eq!(levels(&lines), ["WARN"]);
        assert_eq!(
            lines[0]["fields"]["paperclip_version"].as_str().unwrap(),
            "x".repeat(MAX_LOGGED_CHARS)
        );
    }
}
