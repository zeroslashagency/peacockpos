//! Runtime configuration, read from the environment.
//!
//! Every value has a default that works for local development, so `Config::from_env`
//! never fails on a bare checkout — except for secrets that would be insecure to
//! default. Malformed values are hard errors: silently falling back to a default
//! would hide a broken deployment.
//!
//! S3 Hardening (W4_SECURITY):
//! - `PEACOCK_WEBHOOK_SECRET` is **required**; `test-secret-key` / `CHANGE_ME` / short
//!   values are rejected. The old `unwrap_or("test-secret-key")` fallback in
//!   `routes::aggregators::receive_webhook` is insecure — this config layer fails
//!   closed so the process never starts with that fallback reachable.
//! - `PEACOCK_CORS_ALLOWED_ORIGINS` must not contain `*` — credentials are enabled
//!   so the Fetch spec forbids `Access-Control-Allow-Origin: *`.
//! - `PEACOCK_API_HOST` must not be `0.0.0.0` / `::` (unspecified). The hardened
//!   default is `127.0.0.1:3000` (loopback). Bind to a specific interface
//!   (e.g. Tailscale `100.72.103.1` or `127.0.0.1`) — `0.0.0.0` is rejected.

use std::net::{IpAddr, SocketAddr};

/// How `tracing` output is formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human readable, for local development.
    Pretty,
    /// One JSON object per line, for log shippers.
    Json,
}

impl LogFormat {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pretty" | "text" | "dev" => Ok(Self::Pretty),
            "json" | "prod" | "production" => Ok(Self::Json),
            other => Err(format!(
                "invalid log format {other:?}; expected \"pretty\" or \"json\""
            )),
        }
    }
}

/// Environment variable names, in one place so tests and docs agree.
pub mod env_keys {
    pub const HOST: &str = "PEACOCK_API_HOST";
    pub const PORT: &str = "PEACOCK_API_PORT";
    pub const CORS_ORIGINS: &str = "PEACOCK_CORS_ALLOWED_ORIGINS";
    pub const LOG_FORMAT: &str = "PEACOCK_LOG_FORMAT";
    pub const PROBLEM_BASE_URI: &str = "PEACOCK_PROBLEM_BASE_URI";
    pub const WEBHOOK_SECRET: &str = "PEACOCK_WEBHOOK_SECRET";
    pub const JWT_SECRET: &str = "PEACOCK_JWT_SECRET";
}

#[derive(Clone)]
pub struct Config {
    /// Address the server binds to. Defaults to `127.0.0.1:3000` (loopback, fail-closed).
    /// `0.0.0.0` / `::` is rejected — bind to a specific interface.
    pub bind_addr: SocketAddr,
    /// Exact origins allowed by CORS. Credentials are enabled, so a wildcard is not
    /// legal per the Fetch spec; origins must be listed.
    pub cors_allowed_origins: Vec<String>,
    pub log_format: LogFormat,
    /// Prefix for RFC 7807 `type` URIs.
    pub problem_base_uri: String,
    /// Secret key for HMAC-SHA256 webhook signature validation.
    /// `None` is not allowed via `from_source` — `PEACOCK_WEBHOOK_SECRET` must be set.
    pub webhook_secret: Option<String>,
    /// Secret for HS256 JWTs in `peacock_session` cookie.
    /// Defaults to a dev-only value; set `PEACOCK_JWT_SECRET` in production.
    pub jwt_secret: String,
    /// Price list for COGS calculation (ury_daily_p_and_l.py:30).
    /// Defaults to "Buying". Not from stock valuation — buying prices are configured separately.
    pub buying_price_list: peacock_core::ids::PriceListName,
}

/// Redacted Debug — never prints raw secrets (W4_SECURITY §3c).
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("log_format", &self.log_format)
            .field("problem_base_uri", &self.problem_base_uri)
            .field(
                "webhook_secret",
                &self.webhook_secret.as_deref().map(|_| "<redacted>"),
            )
            .field("jwt_secret", &"<redacted>")
            .field("buying_price_list", &self.buying_price_list)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 3000)),
            cors_allowed_origins: vec![
                "http://localhost:5173".to_string(),
                "http://localhost:3000".to_string(),
            ],
            log_format: LogFormat::Pretty,
            problem_base_uri: "https://peacock-pos.example.com/errors".to_string(),
            webhook_secret: None,
            jwt_secret: "dev-jwt-secret-change-me-in-production".to_string(),
            buying_price_list: peacock_core::ids::PriceListName::from("Buying"),
        }
    }
}

impl Config {
    /// Reads configuration from the process environment.
    ///
    /// # Errors
    /// Returns a message describing the first variable that is present but unusable.
    pub fn from_env() -> Result<Self, String> {
        Self::from_source(|key| std::env::var(key).ok())
    }

    /// Same as [`Config::from_env`] but with an injectable lookup, which keeps the
    /// parsing logic testable without mutating global process state.
    pub fn from_source<F>(get: F) -> Result<Self, String>
    where
        F: Fn(&str) -> Option<String>,
    {
        let defaults = Self::default();

        let host: IpAddr = match get(env_keys::HOST) {
            Some(raw) => raw
                .trim()
                .parse()
                .map_err(|_| format!("invalid {} {:?}", env_keys::HOST, raw))?,
            None => defaults.bind_addr.ip(),
        };

        // S3 Hardening: bind 0.0.0.0 guard — reject unspecified addresses.
        // Default is 127.0.0.1 (loopback, fail-closed). An explicit 0.0.0.0 or :: would
        // expose every endpoint (no auth yet) on all interfaces.
        if host.is_unspecified() {
            return Err(format!(
                "{} must not be 0.0.0.0 or :: — bind to a specific interface (e.g. 127.0.0.1 or Tailscale 100.72.103.1); got {:?}",
                env_keys::HOST, host
            ));
        }

        let port: u16 = match get(env_keys::PORT) {
            Some(raw) => raw
                .trim()
                .parse()
                .map_err(|_| format!("invalid {} {:?}", env_keys::PORT, raw))?,
            None => defaults.bind_addr.port(),
        };

        let cors_allowed_origins = match get(env_keys::CORS_ORIGINS) {
            Some(raw) => {
                let origins: Vec<String> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if origins.is_empty() {
                    return Err(format!("{} is set but empty", env_keys::CORS_ORIGINS));
                }
                // S3 Hardening: CORS star reject — credentials require explicit origins.
                // `HeaderValue::from_str("*")` is valid, but with `allow_credentials(true)`
                // the browser rejects `Access-Control-Allow-Origin: *` and the config is
                // misleading. Reject at startup.
                if origins.iter().any(|o| o == "*") {
                    return Err(format!(
                        "{} must not contain wildcard '*' — credentials require explicit origins (e.g. https://pos.vercel.app)",
                        env_keys::CORS_ORIGINS
                    ));
                }
                if origins.iter().any(|o| o.contains('*')) {
                    return Err(format!(
                        "{} must not contain '*' — wildcard origins are forbidden with credentials",
                        env_keys::CORS_ORIGINS
                    ));
                }
                origins
            }
            None => defaults.cors_allowed_origins,
        };

        let log_format = match get(env_keys::LOG_FORMAT) {
            Some(raw) => LogFormat::parse(&raw)?,
            None => defaults.log_format,
        };

        let problem_base_uri = match get(env_keys::PROBLEM_BASE_URI) {
            Some(raw) => raw.trim().trim_end_matches('/').to_string(),
            None => defaults.problem_base_uri,
        };

        // S3 Hardening: fix HMAC test-secret-key — require PEACOCK_WEBHOOK_SECRET.
        // The handler previously did `unwrap_or("test-secret-key")` which is public in the
        // repo and lets anyone forge `X-Webhook-Signature`. This layer fails closed.
        let webhook_secret = match get(env_keys::WEBHOOK_SECRET)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            None => {
                return Err(format!(
                    "{} must be set — aggregator webhooks require HMAC-SHA256; generate with `openssl rand -hex 32`",
                    env_keys::WEBHOOK_SECRET
                ))
            }
            Some(ref s) if s == "test-secret-key" => {
                return Err(format!(
                    "{} must not be 'test-secret-key' — set a high-entropy value (e.g. `openssl rand -hex 32`)",
                    env_keys::WEBHOOK_SECRET
                ))
            }
            Some(ref s) if s == "CHANGE_ME" => {
                return Err(format!(
                    "{} must not be placeholder 'CHANGE_ME' — set a high-entropy value",
                    env_keys::WEBHOOK_SECRET
                ))
            }
            Some(s) if s.len() < 16 => {
                return Err(format!(
                    "{} must be at least 16 characters (got {}); generate with `openssl rand -hex 32`",
                    env_keys::WEBHOOK_SECRET,
                    s.len()
                ))
            }
            Some(s) => Some(s),
        };

        let jwt_secret = get(env_keys::JWT_SECRET)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or(defaults.jwt_secret);

        let buying_price_list = defaults.buying_price_list;

        Ok(Self {
            bind_addr: SocketAddr::new(host, port),
            cors_allowed_origins,
            log_format,
            problem_base_uri,
            webhook_secret,
            jwt_secret,
            buying_price_list,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        }
    }

    /// Helper that injects a valid high-entropy webhook secret so existing tests focus
    /// on the field they actually exercise.
    fn valid_secret() -> (&'static str, &'static str) {
        (env_keys::WEBHOOK_SECRET, "hardened-webhook-secret-32-chars-ok-123")
    }

    #[test]
    fn defaults_are_hardened_loopback() {
        // Hardened default is 127.0.0.1:3000 (not 0.0.0.0). Caller must provide
        // PEACOCK_WEBHOOK_SECRET, so bare defaults via from_source fail closed.
        let err = Config::from_source(source(&[])).unwrap_err();
        assert!(
            err.contains(env_keys::WEBHOOK_SECRET),
            "bare defaults must require webhook secret, got: {err}"
        );

        // With a valid secret, defaults resolve to loopback.
        let cfg = Config::from_source(source(&[valid_secret()])).expect("defaults with secret are valid");
        assert_eq!(cfg.bind_addr.to_string(), "127.0.0.1:3000");
        assert_eq!(cfg.log_format, LogFormat::Pretty);
    }

    #[test]
    fn env_overrides_bind_addr_and_origins() {
        let cfg = Config::from_source(source(&[
            (env_keys::HOST, "127.0.0.1"),
            (env_keys::PORT, "8080"),
            (
                env_keys::CORS_ORIGINS,
                "https://pos.vercel.app, https://staging.vercel.app",
            ),
            (env_keys::LOG_FORMAT, "json"),
            (env_keys::PROBLEM_BASE_URI, "https://errors.example.com/e/"),
            valid_secret(),
        ]))
        .expect("valid env");

        assert_eq!(cfg.bind_addr.to_string(), "127.0.0.1:8080");
        assert_eq!(
            cfg.cors_allowed_origins,
            vec!["https://pos.vercel.app", "https://staging.vercel.app"]
        );
        assert_eq!(cfg.log_format, LogFormat::Json);
        // Trailing slash is stripped so `type` URIs never contain `//`.
        assert_eq!(cfg.problem_base_uri, "https://errors.example.com/e");
    }

    #[test]
    fn malformed_values_are_rejected_not_defaulted() {
        let port = Config::from_source(source(&[(env_keys::PORT, "not-a-port"), valid_secret()]));
        assert!(port.unwrap_err().contains(env_keys::PORT));

        let host = Config::from_source(source(&[(env_keys::HOST, "nope"), valid_secret()]));
        assert!(host.unwrap_err().contains(env_keys::HOST));

        let format = Config::from_source(source(&[(env_keys::LOG_FORMAT, "yaml"), valid_secret()]));
        assert!(format.unwrap_err().contains("invalid log format"));

        let origins = Config::from_source(source(&[(env_keys::CORS_ORIGINS, " , "), valid_secret()]));
        assert!(origins.unwrap_err().contains("empty"));
    }

    #[test]
    fn cors_wildcard_star_is_rejected() {
        let star = Config::from_source(source(&[(env_keys::CORS_ORIGINS, "*"), valid_secret()]));
        assert!(
            star.unwrap_err().contains("wildcard"),
            "CORS '*' must be rejected"
        );

        let mixed = Config::from_source(source(&[
            (env_keys::CORS_ORIGINS, "https://pos.vercel.app, *"),
            valid_secret(),
        ]));
        assert!(mixed.unwrap_err().contains("wildcard"));

        let glob = Config::from_source(source(&[
            (env_keys::CORS_ORIGINS, "https://*.vercel.app"),
            valid_secret(),
        ]));
        assert!(glob.unwrap_err().contains('*'.to_string().as_str()));
    }

    #[test]
    fn bind_unspecified_is_rejected() {
        let v4 = Config::from_source(source(&[(env_keys::HOST, "0.0.0.0"), valid_secret()]));
        let err = v4.unwrap_err();
        assert!(
            err.contains("0.0.0.0") || err.contains("unspecified"),
            "0.0.0.0 must be rejected, got: {err}"
        );
        assert!(err.contains(env_keys::HOST));

        let v6 = Config::from_source(source(&[(env_keys::HOST, "::"), valid_secret()]));
        let err = v6.unwrap_err();
        assert!(
            err.contains("::") || err.contains("unspecified"),
            ":: must be rejected, got: {err}"
        );
    }

    #[test]
    fn webhook_secret_is_required() {
        let missing = Config::from_source(source(&[(env_keys::HOST, "127.0.0.1")]));
        let err = missing.unwrap_err();
        assert!(
            err.contains(env_keys::WEBHOOK_SECRET),
            "missing webhook secret must be rejected, got: {err}"
        );
    }

    #[test]
    fn webhook_secret_rejects_test_key_and_placeholder() {
        let test_key = Config::from_source(source(&[
            (env_keys::WEBHOOK_SECRET, "test-secret-key"),
            (env_keys::HOST, "127.0.0.1"),
        ]));
        assert!(
            test_key.unwrap_err().contains("test-secret-key"),
            "test-secret-key must be rejected"
        );

        let placeholder = Config::from_source(source(&[
            (env_keys::WEBHOOK_SECRET, "CHANGE_ME"),
            (env_keys::HOST, "127.0.0.1"),
        ]));
        assert!(placeholder.unwrap_err().contains("CHANGE_ME"));

        let short = Config::from_source(source(&[
            (env_keys::WEBHOOK_SECRET, "short"),
            (env_keys::HOST, "127.0.0.1"),
        ]));
        assert!(short.unwrap_err().contains("at least 16"));
    }

    #[test]
    fn webhook_secret_redacted_in_debug() {
        let cfg = Config {
            webhook_secret: Some("super-secret-value-32-chars-long-ok".to_string()),
            jwt_secret: "also-secret".to_string(),
            ..Config::default()
        };
        let dumped = format!("{cfg:?}");
        assert!(!dumped.contains("super-secret"), "webhook secret leaked in Debug: {dumped}");
        assert!(!dumped.contains("also-secret"), "jwt secret leaked in Debug: {dumped}");
        assert!(dumped.contains("<redacted>"), "Debug must contain <redacted>");
    }
}
