//! Configuration from environment variables (architecture §6.1).
//!
//! Secrets are read only from files (`*_FILE`). Error messages name the
//! variable and the reason, never the value.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use url::Url;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{0} is required but not set")]
    Missing(&'static str),
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
    #[error("{var}: secret file {reason}")]
    SecretFile { var: &'static str, reason: String },
    /// A v1 variable whose meaning cannot be carried over (ADR-0010 §2.7).
    #[error("{var} is no longer supported in v2: {hint}")]
    Removed {
        var: &'static str,
        hint: &'static str,
    },
}

/// v1 variables that stop the start: silently ignoring them would change
/// who can talk to the bridge or which companies it serves.
pub const REMOVED_VARS: [(&str, &str); 2] = [
    (
        "PAPERCLIP_COMPANY_IDS",
        "companies are discovered automatically, remove the variable",
    ),
    (
        "TELEGRAM_USER_ID",
        "use TELEGRAM_ALLOWED_USER_IDS (comma-separated Telegram user ids) and set TELEGRAM_CHAT_ID to the forum supergroup",
    ),
];

/// v1 variables without a v2 meaning: a WARN at start, nothing else.
pub const IGNORED_VARS: [&str; 9] = [
    "PAPERCLIP_RUNS_POLL_INTERVAL",
    "PAPERCLIP_GOVERNED_TIMEOUT",
    "GOVERNED_APPROVALS_ENABLED",
    "GOVERNED_DESTRUCTIVE_MODE",
    "GOVERNED_SHOW_ARGUMENTS",
    "GOVERNED_RESULT_FOLLOW",
    "NOTIFY_RESOLVED_ELSEWHERE",
    "NOTIFY_RUN_FAILURES",
    "NOTIFY_TASK_COMPLETED",
];

/// Proxy URL for Telegram. May contain credentials, so it is kept secret and
/// only the scheme is ever printed.
#[derive(Clone)]
pub struct ProxyUrl {
    url: SecretString,
    scheme: String,
}

impl ProxyUrl {
    pub const ALLOWED_SCHEMES: [&'static str; 4] = ["http", "https", "socks5", "socks5h"];

    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("must not be empty".into());
        }
        let parsed = Url::parse(raw).map_err(|_| "not a valid URL".to_string())?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        if !Self::ALLOWED_SCHEMES.contains(&scheme.as_str()) {
            return Err("scheme must be one of http, https, socks5, socks5h".into());
        }
        if parsed.host_str().is_none_or(str::is_empty) {
            return Err("host is missing".into());
        }
        Ok(Self {
            url: SecretString::from(raw.to_string()),
            scheme,
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn expose(&self) -> &str {
        self.url.expose_secret()
    }
}

impl fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProxyUrl({}://[REDACTED])", self.scheme)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Text,
}

#[derive(Debug, Clone)]
pub struct PaperclipConfig {
    pub url: Url,
    /// One board key (`PAPERCLIP_TOKEN_FILE`) or several (`PAPERCLIP_TOKEN_FILES`),
    /// in configuration order (ADR-0010 §1.1).
    pub tokens: Vec<SecretString>,
    pub ui_url: Option<Url>,
    /// `/chats` of every company and hot conversations.
    pub poll_interval: Duration,
    /// Company list and agent names.
    pub discovery_interval: Duration,
    /// Safety read of cold conversations.
    pub sweep_interval: Duration,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: SecretString,
    /// The forum supergroup (`-100…`).
    pub chat_id: i64,
    pub allowed_user_ids: Vec<i64>,
    /// Mandatory: there is no code path that talks to Telegram without it.
    pub proxy: ProxyUrl,
    pub api_base: Url,
    pub long_poll_timeout: Duration,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    /// Pause between chat-changing calls: 20 messages a minute per group.
    pub group_min_interval: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
    pub notify_after: u32,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub paperclip: PaperclipConfig,
    pub telegram: TelegramConfig,
    pub state_path: PathBuf,
    pub health: HealthConfig,
    pub retry: RetryConfig,
    /// A conversation stays "hot" (comments read every poll) this long after
    /// any activity (ADR-0010 §2.3).
    pub hot_window: Duration,
    pub max_message_chunks: usize,
    pub shutdown_grace: Duration,
    pub log: LogConfig,
    /// Variables of v1 that are set but no longer used (WARN at start).
    pub ignored_vars: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub struct HealthConfig {
    pub file: PathBuf,
    pub max_stale: Duration,
}

#[derive(Debug, Clone)]
pub struct LogConfig {
    pub level: tracing::Level,
    pub format: LogFormat,
}

/// Source of variables: the process environment in production, a map in tests.
pub struct Env<'a> {
    vars: &'a HashMap<String, String>,
}

impl<'a> Env<'a> {
    pub fn new(vars: &'a HashMap<String, String>) -> Self {
        Self { vars }
    }

    fn get(&self, var: &'static str) -> Option<&str> {
        self.vars
            .get(var)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }

    fn required(&self, var: &'static str) -> Result<&str, ConfigError> {
        self.get(var).ok_or(ConfigError::Missing(var))
    }

    fn duration(&self, var: &'static str, default: &str) -> Result<Duration, ConfigError> {
        let raw = self.get(var).unwrap_or(default);
        let d = humantime::parse_duration(raw).map_err(|_| ConfigError::Invalid {
            var,
            reason: "expected a duration like 15s or 2m".into(),
        })?;
        if d.is_zero() {
            return Err(ConfigError::Invalid {
                var,
                reason: "must be greater than zero".into(),
            });
        }
        Ok(d)
    }

    fn uint(&self, var: &'static str, default: u32, min: u32) -> Result<u32, ConfigError> {
        let v = match self.get(var) {
            None => default,
            Some(raw) => raw.parse::<u32>().map_err(|_| ConfigError::Invalid {
                var,
                reason: "expected a non-negative integer".into(),
            })?,
        };
        if v < min {
            return Err(ConfigError::Invalid {
                var,
                reason: format!("must be at least {min}"),
            });
        }
        Ok(v)
    }

    fn int_id(&self, var: &'static str) -> Result<i64, ConfigError> {
        self.required(var)?
            .parse::<i64>()
            .map_err(|_| ConfigError::Invalid {
                var,
                reason: "expected an integer id".into(),
            })
    }

    fn id_list(&self, var: &'static str) -> Result<Vec<i64>, ConfigError> {
        let ids = self
            .required(var)?
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<i64>().map_err(|_| ConfigError::Invalid {
                    var,
                    reason: "expected a comma-separated list of integer ids".into(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if ids.is_empty() {
            return Err(ConfigError::Invalid {
                var,
                reason: "expected at least one id".into(),
            });
        }
        Ok(ids)
    }

    fn http_url(&self, var: &'static str, raw: &str) -> Result<Url, ConfigError> {
        let url = Url::parse(raw).map_err(|_| ConfigError::Invalid {
            var,
            reason: "not a valid URL".into(),
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(ConfigError::Invalid {
                var,
                reason: "expected an http:// or https:// URL".into(),
            });
        }
        Ok(url)
    }

    fn secret_file(&self, var: &'static str) -> Result<SecretString, ConfigError> {
        let path = self.required(var)?;
        read_secret_file(var, Path::new(path))
    }

    /// `PAPERCLIP_TOKEN_FILE` (option A) or `PAPERCLIP_TOKEN_FILES` (option B),
    /// never both (ADR-0010 §1.1).
    fn paperclip_tokens(&self) -> Result<Vec<SecretString>, ConfigError> {
        const ONE: &str = "PAPERCLIP_TOKEN_FILE";
        const MANY: &str = "PAPERCLIP_TOKEN_FILES";
        match (self.get(ONE), self.get(MANY)) {
            (Some(_), Some(_)) => Err(ConfigError::Invalid {
                var: MANY,
                reason: format!("must not be set together with {ONE}"),
            }),
            (Some(_), None) => Ok(vec![self.secret_file(ONE)?]),
            (None, Some(list)) => {
                let paths: Vec<&str> = list
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .collect();
                if paths.is_empty() {
                    return Err(ConfigError::Invalid {
                        var: MANY,
                        reason: "expected a comma-separated list of file paths".into(),
                    });
                }
                let mut seen = std::collections::HashSet::new();
                let mut tokens = Vec::with_capacity(paths.len());
                for p in paths {
                    if !seen.insert(p) {
                        return Err(ConfigError::Invalid {
                            var: MANY,
                            reason: "the same file is listed twice".into(),
                        });
                    }
                    tokens.push(read_secret_file(MANY, Path::new(p))?);
                }
                Ok(tokens)
            }
            (None, None) => Err(ConfigError::Missing(ONE)),
        }
    }
}

pub fn read_secret_file(var: &'static str, path: &Path) -> Result<SecretString, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|e| ConfigError::SecretFile {
        var,
        reason: format!("cannot be read ({})", e.kind()),
    })?;
    let value = content.trim();
    if value.is_empty() {
        return Err(ConfigError::SecretFile {
            var,
            reason: "is empty".into(),
        });
    }
    if value.contains(['\n', '\r']) {
        return Err(ConfigError::SecretFile {
            var,
            reason: "must contain a single line".into(),
        });
    }
    Ok(SecretString::from(value.to_string()))
}

impl Config {
    pub fn from_process_env() -> Result<Self, ConfigError> {
        let vars: HashMap<String, String> = std::env::vars().collect();
        Self::from_env(&Env::new(&vars))
    }

    pub fn from_env(env: &Env<'_>) -> Result<Self, ConfigError> {
        for (var, hint) in REMOVED_VARS {
            if env.get(var).is_some() {
                return Err(ConfigError::Removed { var, hint });
            }
        }
        // Proxy first: without it nothing else matters (architecture §5.1).
        let proxy = ProxyUrl::parse(env.required("TELEGRAM_PROXY_URL")?).map_err(|reason| {
            ConfigError::Invalid {
                var: "TELEGRAM_PROXY_URL",
                reason,
            }
        })?;

        let paperclip_url = env.http_url("PAPERCLIP_URL", env.required("PAPERCLIP_URL")?)?;
        let ui_url = match env.get("PAPERCLIP_UI_URL") {
            Some(raw) => Some(env.http_url("PAPERCLIP_UI_URL", raw)?),
            None => None,
        };

        let api_base = env.http_url(
            "TELEGRAM_API_BASE_URL",
            env.get("TELEGRAM_API_BASE_URL")
                .unwrap_or("https://api.telegram.org"),
        )?;

        let long_poll_timeout = env.duration("TELEGRAM_LONG_POLL_TIMEOUT", "25s")?;
        if long_poll_timeout > Duration::from_secs(50) {
            return Err(ConfigError::Invalid {
                var: "TELEGRAM_LONG_POLL_TIMEOUT",
                reason: "must not exceed 50s".into(),
            });
        }
        let connect_timeout = env.duration("CONNECT_TIMEOUT", "10s")?;

        let chat_id = env.int_id("TELEGRAM_CHAT_ID")?;
        if chat_id >= 0 {
            return Err(ConfigError::Invalid {
                var: "TELEGRAM_CHAT_ID",
                reason: "expected the id of a supergroup (negative, -100…)".into(),
            });
        }

        let level = match env.get("LOG_LEVEL").map(str::to_ascii_lowercase).as_deref() {
            None | Some("info") => tracing::Level::INFO,
            Some("error") => tracing::Level::ERROR,
            Some("warn") => tracing::Level::WARN,
            Some("debug") => tracing::Level::DEBUG,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "LOG_LEVEL",
                    reason: "expected error, warn, info or debug".into(),
                });
            }
        };
        let format = match env
            .get("LOG_FORMAT")
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("json") => LogFormat::Json,
            Some("text") => LogFormat::Text,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "LOG_FORMAT",
                    reason: "expected json or text".into(),
                });
            }
        };

        let retry = RetryConfig {
            base_delay: env.duration("RETRY_BASE_DELAY", "1s")?,
            max_delay: env.duration("RETRY_MAX_DELAY", "60s")?,
            max_attempts: env.uint("RETRY_MAX_ATTEMPTS", 6, 1)?,
            notify_after: env.uint("RETRY_NOTIFY_AFTER", 5, 1)?,
        };
        if retry.max_delay < retry.base_delay {
            return Err(ConfigError::Invalid {
                var: "RETRY_MAX_DELAY",
                reason: "must not be less than RETRY_BASE_DELAY".into(),
            });
        }

        Ok(Config {
            paperclip: PaperclipConfig {
                url: paperclip_url,
                tokens: env.paperclip_tokens()?,
                ui_url,
                poll_interval: env.duration("PAPERCLIP_POLL_INTERVAL", "5s")?,
                discovery_interval: env.duration("PAPERCLIP_DISCOVERY_INTERVAL", "60s")?,
                sweep_interval: env.duration("PAPERCLIP_SWEEP_INTERVAL", "60s")?,
                request_timeout: env.duration("PAPERCLIP_REQUEST_TIMEOUT", "15s")?,
                connect_timeout,
            },
            telegram: TelegramConfig {
                token: env.secret_file("TELEGRAM_BOT_TOKEN_FILE")?,
                chat_id,
                allowed_user_ids: env.id_list("TELEGRAM_ALLOWED_USER_IDS")?,
                proxy,
                api_base,
                long_poll_timeout,
                request_timeout: env.duration("TELEGRAM_REQUEST_TIMEOUT", "15s")?,
                connect_timeout,
                group_min_interval: env.duration("TELEGRAM_GROUP_MIN_INTERVAL", "3s")?,
            },
            state_path: PathBuf::from(env.get("STATE_PATH").unwrap_or("/state/bridge.db")),
            health: HealthConfig::from_env(env)?,
            retry,
            hot_window: env.duration("CONVERSATION_HOT_WINDOW", "10m")?,
            max_message_chunks: env.uint("MAX_MESSAGE_CHUNKS", 3, 1)? as usize,
            shutdown_grace: env.duration("SHUTDOWN_GRACE", "10s")?,
            log: LogConfig { level, format },
            ignored_vars: IGNORED_VARS
                .into_iter()
                .filter(|v| env.get(v).is_some())
                .collect(),
        })
    }
}

impl HealthConfig {
    /// Only what `bridge healthcheck` needs, so the check never fails because
    /// of an unrelated variable.
    pub fn from_env(env: &Env<'_>) -> Result<Self, ConfigError> {
        Ok(Self {
            file: PathBuf::from(env.get("HEALTH_FILE").unwrap_or("/state/health.json")),
            max_stale: env.duration("HEALTH_MAX_STALE", "5m")?,
        })
    }
}

/// Minimal config for `bridge revoke-paperclip-key`: one key, from
/// `PAPERCLIP_TOKEN_FILE` (with option B, point it at the key to revoke).
pub struct RevokeConfig {
    pub url: Url,
    pub token: SecretString,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
}

impl RevokeConfig {
    pub fn from_env(env: &Env<'_>) -> Result<Self, ConfigError> {
        Ok(Self {
            url: env.http_url("PAPERCLIP_URL", env.required("PAPERCLIP_URL")?)?,
            token: env.secret_file("PAPERCLIP_TOKEN_FILE")?,
            request_timeout: env.duration("PAPERCLIP_REQUEST_TIMEOUT", "15s")?,
            connect_timeout: env.duration("CONNECT_TIMEOUT", "10s")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Fixture {
        dir: tempfile::TempDir,
        vars: HashMap<String, String>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let pc = dir.path().join("pc");
        let tg = dir.path().join("tg");
        std::fs::write(&pc, "pcp_board_placeholder\n").unwrap();
        std::fs::write(&tg, "123456:placeholder-token\n").unwrap();
        let mut vars = HashMap::new();
        for (k, v) in [
            ("PAPERCLIP_URL", "http://paperclip.internal:3100"),
            ("TELEGRAM_CHAT_ID", "-1001234567890"),
            ("TELEGRAM_ALLOWED_USER_IDS", "123456789"),
            ("TELEGRAM_PROXY_URL", "socks5h://xray:1080"),
        ] {
            vars.insert(k.to_string(), v.to_string());
        }
        vars.insert(
            "PAPERCLIP_TOKEN_FILE".into(),
            pc.to_string_lossy().into_owned(),
        );
        vars.insert(
            "TELEGRAM_BOT_TOKEN_FILE".into(),
            tg.to_string_lossy().into_owned(),
        );
        Fixture { dir, vars }
    }

    fn parse(vars: &HashMap<String, String>) -> Result<Config, ConfigError> {
        Config::from_env(&Env::new(vars))
    }

    #[test]
    fn parses_minimal_config_with_defaults() {
        let f = fixture();
        let c = parse(&f.vars).unwrap();
        assert_eq!(c.paperclip.tokens.len(), 1);
        assert_eq!(c.paperclip.poll_interval, Duration::from_secs(5));
        assert_eq!(c.paperclip.discovery_interval, Duration::from_secs(60));
        assert_eq!(c.paperclip.sweep_interval, Duration::from_secs(60));
        assert_eq!(c.hot_window, Duration::from_secs(600));
        assert_eq!(c.telegram.chat_id, -1001234567890);
        assert_eq!(c.telegram.allowed_user_ids, vec![123456789]);
        assert_eq!(c.telegram.group_min_interval, Duration::from_secs(3));
        assert_eq!(c.telegram.long_poll_timeout, Duration::from_secs(25));
        assert_eq!(c.telegram.api_base.as_str(), "https://api.telegram.org/");
        assert_eq!(c.telegram.proxy.scheme(), "socks5h");
        assert_eq!(c.max_message_chunks, 3);
        assert_eq!(c.state_path, PathBuf::from("/state/bridge.db"));
        assert_eq!(c.health.max_stale, Duration::from_secs(300));
        assert!(c.paperclip.ui_url.is_none());
        assert!(c.ignored_vars.is_empty());
        assert_eq!(
            c.paperclip.tokens[0].expose_secret(),
            "pcp_board_placeholder"
        );
    }

    #[test]
    fn several_keys_and_users() {
        let mut f = fixture();
        let second = f.dir.path().join("pc2");
        std::fs::write(&second, "pcp_second_placeholder").unwrap();
        let first = f.vars.remove("PAPERCLIP_TOKEN_FILE").unwrap();
        f.vars.insert(
            "PAPERCLIP_TOKEN_FILES".into(),
            format!("{first}, {}", second.to_string_lossy()),
        );
        f.vars
            .insert("TELEGRAM_ALLOWED_USER_IDS".into(), "1, 22 ,333".into());
        let c = parse(&f.vars).unwrap();
        let keys: Vec<&str> = c
            .paperclip
            .tokens
            .iter()
            .map(|t| t.expose_secret())
            .collect();
        assert_eq!(keys, ["pcp_board_placeholder", "pcp_second_placeholder"]);
        assert_eq!(c.telegram.allowed_user_ids, vec![1, 22, 333]);
    }

    #[test]
    fn key_variables_are_mutually_exclusive() {
        let mut f = fixture();
        let one = f.vars["PAPERCLIP_TOKEN_FILE"].clone();
        f.vars.insert("PAPERCLIP_TOKEN_FILES".into(), one.clone());
        let err = parse(&f.vars).unwrap_err();
        assert!(err.to_string().contains("PAPERCLIP_TOKEN_FILE"), "{err}");
        f.vars.remove("PAPERCLIP_TOKEN_FILE");
        f.vars
            .insert("PAPERCLIP_TOKEN_FILES".into(), format!("{one},{one}"));
        assert!(parse(&f.vars).unwrap_err().to_string().contains("twice"));
        f.vars.remove("PAPERCLIP_TOKEN_FILES");
        assert_eq!(
            parse(&f.vars).unwrap_err(),
            ConfigError::Missing("PAPERCLIP_TOKEN_FILE")
        );
    }

    #[test]
    fn removed_v1_variables_stop_the_start_with_a_hint() {
        for (var, hint) in [
            ("PAPERCLIP_COMPANY_IDS", "discovered automatically"),
            ("TELEGRAM_USER_ID", "TELEGRAM_ALLOWED_USER_IDS"),
        ] {
            let mut f = fixture();
            f.vars.insert(var.into(), "123".into());
            let err = parse(&f.vars).unwrap_err();
            assert!(matches!(err, ConfigError::Removed { .. }), "{err}");
            let text = err.to_string();
            assert!(text.contains(var) && text.contains(hint), "{text}");
        }
    }

    #[test]
    fn obsolete_v1_variables_are_only_reported() {
        let mut f = fixture();
        f.vars
            .insert("NOTIFY_RUN_FAILURES".into(), "not-a-bool".into());
        f.vars
            .insert("GOVERNED_DESTRUCTIVE_MODE".into(), "ui_only".into());
        let c = parse(&f.vars).unwrap();
        assert_eq!(
            c.ignored_vars,
            vec!["GOVERNED_DESTRUCTIVE_MODE", "NOTIFY_RUN_FAILURES"]
        );
    }

    #[test]
    fn missing_proxy_is_rejected() {
        let mut f = fixture();
        f.vars.remove("TELEGRAM_PROXY_URL");
        assert_eq!(
            parse(&f.vars).unwrap_err(),
            ConfigError::Missing("TELEGRAM_PROXY_URL")
        );
        f.vars.insert("TELEGRAM_PROXY_URL".into(), "  ".into());
        assert_eq!(
            parse(&f.vars).unwrap_err(),
            ConfigError::Missing("TELEGRAM_PROXY_URL")
        );
    }

    #[test]
    fn proxy_scheme_is_validated_without_echoing_value() {
        let mut f = fixture();
        for bad in [
            "ftp://user:hunter2@xray:21",
            "xray:1080",
            "socks4://xray:1080",
            "http://",
        ] {
            f.vars.insert("TELEGRAM_PROXY_URL".into(), bad.into());
            let err = parse(&f.vars).unwrap_err();
            let text = err.to_string();
            assert!(text.contains("TELEGRAM_PROXY_URL"), "{text}");
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains("xray"), "{text}");
        }
        for good in [
            "http://xray:8080",
            "https://xray:8443",
            "socks5://u:p@xray:1080",
            "SOCKS5H://xray:1080",
        ] {
            f.vars.insert("TELEGRAM_PROXY_URL".into(), good.into());
            parse(&f.vars).unwrap();
        }
    }

    #[test]
    fn proxy_debug_hides_credentials() {
        let p = ProxyUrl::parse("socks5h://user:hunter2@xray:1080").unwrap();
        let dbg = format!("{p:?}");
        assert_eq!(dbg, "ProxyUrl(socks5h://[REDACTED])");
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let f = fixture();
        let c = parse(&f.vars).unwrap();
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("pcp_board_placeholder"));
        assert!(!dbg.contains("placeholder-token"));
        assert!(dbg.contains("REDACTED"));
    }

    #[test]
    fn empty_secret_file_is_rejected() {
        let f = fixture();
        let path = f.vars["PAPERCLIP_TOKEN_FILE"].clone();
        std::fs::write(&path, "\n  \n").unwrap();
        assert!(matches!(
            parse(&f.vars).unwrap_err(),
            ConfigError::SecretFile {
                var: "PAPERCLIP_TOKEN_FILE",
                ..
            }
        ));
    }

    #[test]
    fn missing_secret_file_is_rejected() {
        let mut f = fixture();
        f.vars
            .insert("TELEGRAM_BOT_TOKEN_FILE".into(), "/nonexistent/x".into());
        let err = parse(&f.vars).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::SecretFile {
                var: "TELEGRAM_BOT_TOKEN_FILE",
                ..
            }
        ));
        assert!(!err.to_string().contains("/nonexistent"));
    }

    #[test]
    fn secret_file_trims_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"abc\r\n").unwrap();
        let s = read_secret_file("X_FILE", &p).unwrap();
        assert_eq!(s.expose_secret(), "abc");
    }

    #[test]
    fn invalid_values_name_the_variable() {
        let cases = [
            ("PAPERCLIP_POLL_INTERVAL", "soon"),
            ("PAPERCLIP_POLL_INTERVAL", "0s"),
            ("PAPERCLIP_DISCOVERY_INTERVAL", "never"),
            ("PAPERCLIP_SWEEP_INTERVAL", "0s"),
            ("CONVERSATION_HOT_WINDOW", "hot"),
            ("TELEGRAM_GROUP_MIN_INTERVAL", "fast"),
            ("TELEGRAM_LONG_POLL_TIMEOUT", "60s"),
            ("TELEGRAM_CHAT_ID", "me"),
            ("TELEGRAM_CHAT_ID", "123456789"),
            ("TELEGRAM_ALLOWED_USER_IDS", " , "),
            ("TELEGRAM_ALLOWED_USER_IDS", "1,me"),
            ("LOG_LEVEL", "trace"),
            ("LOG_FORMAT", "xml"),
            ("MAX_MESSAGE_CHUNKS", "0"),
            ("PAPERCLIP_URL", "ftp://paperclip"),
            ("RETRY_MAX_DELAY", "100ms"),
        ];
        for (var, value) in cases {
            let mut f = fixture();
            f.vars.insert(var.into(), value.into());
            let err = parse(&f.vars).expect_err(var);
            assert!(err.to_string().contains(var), "{var}: {err}");
        }
    }

    #[test]
    fn optional_values_are_parsed() {
        let mut f = fixture();
        for (k, v) in [
            ("PAPERCLIP_UI_URL", "https://paperclip.example"),
            ("LOG_FORMAT", "text"),
            ("LOG_LEVEL", "debug"),
            ("PAPERCLIP_POLL_INTERVAL", "2m"),
            ("CONVERSATION_HOT_WINDOW", "1h"),
            ("TELEGRAM_GROUP_MIN_INTERVAL", "500ms"),
            ("BRIDGE_UNKNOWN", "ignored"),
        ] {
            f.vars.insert(k.into(), v.into());
        }
        let c = parse(&f.vars).unwrap();
        assert!(c.paperclip.ui_url.is_some());
        assert_eq!(c.log.format, LogFormat::Text);
        assert_eq!(c.paperclip.poll_interval, Duration::from_secs(120));
        assert_eq!(c.hot_window, Duration::from_secs(3600));
        assert_eq!(c.telegram.group_min_interval, Duration::from_millis(500));
    }
}
