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
}

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
pub enum DestructiveMode {
    Confirm,
    UiOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Text,
}

#[derive(Debug, Clone)]
pub struct PaperclipConfig {
    pub url: Url,
    pub token: SecretString,
    pub company_ids: Vec<String>,
    pub ui_url: Option<Url>,
    pub poll_interval: Duration,
    pub runs_poll_interval: Duration,
    pub request_timeout: Duration,
    pub governed_timeout: Duration,
    pub connect_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: SecretString,
    pub chat_id: i64,
    pub user_id: i64,
    /// Mandatory: there is no code path that talks to Telegram without it.
    pub proxy: ProxyUrl,
    pub api_base: Url,
    pub long_poll_timeout: Duration,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
    pub notify_after: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct NotifyConfig {
    pub run_failures: bool,
    pub task_completed: bool,
    pub resolved_elsewhere: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct GovernedConfig {
    pub enabled: bool,
    pub destructive_mode: DestructiveMode,
    pub show_arguments: bool,
    pub result_follow: Duration,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub paperclip: PaperclipConfig,
    pub telegram: TelegramConfig,
    pub state_path: PathBuf,
    pub health: HealthConfig,
    pub retry: RetryConfig,
    pub notify: NotifyConfig,
    pub governed: GovernedConfig,
    pub max_message_chunks: usize,
    pub shutdown_grace: Duration,
    pub log: LogConfig,
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

    fn boolean(&self, var: &'static str, default: bool) -> Result<bool, ConfigError> {
        match self.get(var).map(str::to_ascii_lowercase).as_deref() {
            None => Ok(default),
            Some("true" | "1" | "yes") => Ok(true),
            Some("false" | "0" | "no") => Ok(false),
            Some(_) => Err(ConfigError::Invalid {
                var,
                reason: "expected true or false".into(),
            }),
        }
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
        let company_ids: Vec<String> = env
            .required("PAPERCLIP_COMPANY_IDS")?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if company_ids.is_empty() {
            return Err(ConfigError::Invalid {
                var: "PAPERCLIP_COMPANY_IDS",
                reason: "expected a comma-separated list of company ids".into(),
            });
        }
        if company_ids.iter().any(|id| {
            !id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        }) {
            return Err(ConfigError::Invalid {
                var: "PAPERCLIP_COMPANY_IDS",
                reason: "ids may contain only letters, digits, '-' and '_'".into(),
            });
        }

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

        let destructive_mode = match env
            .get("GOVERNED_DESTRUCTIVE_MODE")
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("confirm") => DestructiveMode::Confirm,
            Some("ui_only") => DestructiveMode::UiOnly,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "GOVERNED_DESTRUCTIVE_MODE",
                    reason: "expected confirm or ui_only".into(),
                });
            }
        };

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
                token: env.secret_file("PAPERCLIP_TOKEN_FILE")?,
                company_ids,
                ui_url,
                poll_interval: env.duration("PAPERCLIP_POLL_INTERVAL", "15s")?,
                runs_poll_interval: env.duration("PAPERCLIP_RUNS_POLL_INTERVAL", "60s")?,
                request_timeout: env.duration("PAPERCLIP_REQUEST_TIMEOUT", "15s")?,
                governed_timeout: env.duration("PAPERCLIP_GOVERNED_TIMEOUT", "120s")?,
                connect_timeout,
            },
            telegram: TelegramConfig {
                token: env.secret_file("TELEGRAM_BOT_TOKEN_FILE")?,
                chat_id: env.int_id("TELEGRAM_CHAT_ID")?,
                user_id: env.int_id("TELEGRAM_USER_ID")?,
                proxy,
                api_base,
                long_poll_timeout,
                request_timeout: env.duration("TELEGRAM_REQUEST_TIMEOUT", "15s")?,
                connect_timeout,
            },
            state_path: PathBuf::from(env.get("STATE_PATH").unwrap_or("/state/bridge.db")),
            health: HealthConfig::from_env(env)?,
            retry,
            notify: NotifyConfig {
                run_failures: env.boolean("NOTIFY_RUN_FAILURES", true)?,
                task_completed: env.boolean("NOTIFY_TASK_COMPLETED", false)?,
                resolved_elsewhere: env.boolean("NOTIFY_RESOLVED_ELSEWHERE", true)?,
            },
            governed: GovernedConfig {
                enabled: env.boolean("GOVERNED_APPROVALS_ENABLED", true)?,
                destructive_mode,
                show_arguments: env.boolean("GOVERNED_SHOW_ARGUMENTS", false)?,
                result_follow: env.duration("GOVERNED_RESULT_FOLLOW", "30m")?,
            },
            max_message_chunks: env.uint("MAX_MESSAGE_CHUNKS", 3, 1)? as usize,
            shutdown_grace: env.duration("SHUTDOWN_GRACE", "10s")?,
            log: LogConfig { level, format },
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

/// Minimal config for `bridge revoke-paperclip-key`.
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
        _dir: tempfile::TempDir,
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
            ("PAPERCLIP_COMPANY_IDS", "company-a, company-b"),
            ("TELEGRAM_CHAT_ID", "123456789"),
            ("TELEGRAM_USER_ID", "123456789"),
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
        Fixture { _dir: dir, vars }
    }

    fn parse(vars: &HashMap<String, String>) -> Result<Config, ConfigError> {
        Config::from_env(&Env::new(vars))
    }

    #[test]
    fn parses_minimal_config_with_defaults() {
        let f = fixture();
        let c = parse(&f.vars).unwrap();
        assert_eq!(c.paperclip.company_ids, vec!["company-a", "company-b"]);
        assert_eq!(c.paperclip.poll_interval, Duration::from_secs(15));
        assert_eq!(c.paperclip.governed_timeout, Duration::from_secs(120));
        assert_eq!(c.telegram.long_poll_timeout, Duration::from_secs(25));
        assert_eq!(c.telegram.api_base.as_str(), "https://api.telegram.org/");
        assert_eq!(c.telegram.proxy.scheme(), "socks5h");
        assert_eq!(c.governed.destructive_mode, DestructiveMode::Confirm);
        assert!(c.governed.enabled);
        assert!(!c.governed.show_arguments);
        assert!(c.notify.run_failures);
        assert!(!c.notify.task_completed);
        assert_eq!(c.max_message_chunks, 3);
        assert_eq!(c.state_path, PathBuf::from("/state/bridge.db"));
        assert_eq!(c.health.max_stale, Duration::from_secs(300));
        assert!(c.paperclip.ui_url.is_none());
        assert_eq!(c.paperclip.token.expose_secret(), "pcp_board_placeholder");
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
            ("TELEGRAM_LONG_POLL_TIMEOUT", "60s"),
            ("TELEGRAM_CHAT_ID", "me"),
            ("GOVERNED_DESTRUCTIVE_MODE", "yolo"),
            ("NOTIFY_RUN_FAILURES", "maybe"),
            ("LOG_LEVEL", "trace"),
            ("LOG_FORMAT", "xml"),
            ("MAX_MESSAGE_CHUNKS", "0"),
            ("PAPERCLIP_URL", "ftp://paperclip"),
            ("PAPERCLIP_COMPANY_IDS", " , "),
            ("PAPERCLIP_COMPANY_IDS", "a/b"),
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
            ("GOVERNED_DESTRUCTIVE_MODE", "ui_only"),
            ("GOVERNED_APPROVALS_ENABLED", "false"),
            ("GOVERNED_SHOW_ARGUMENTS", "true"),
            ("PAPERCLIP_UI_URL", "https://paperclip.example"),
            ("LOG_FORMAT", "text"),
            ("LOG_LEVEL", "debug"),
            ("PAPERCLIP_POLL_INTERVAL", "2m"),
            ("BRIDGE_UNKNOWN", "ignored"),
        ] {
            f.vars.insert(k.into(), v.into());
        }
        let c = parse(&f.vars).unwrap();
        assert_eq!(c.governed.destructive_mode, DestructiveMode::UiOnly);
        assert!(!c.governed.enabled);
        assert!(c.governed.show_arguments);
        assert!(c.paperclip.ui_url.is_some());
        assert_eq!(c.log.format, LogFormat::Text);
        assert_eq!(c.paperclip.poll_interval, Duration::from_secs(120));
    }
}
