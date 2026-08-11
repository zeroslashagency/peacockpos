//! Runtime configuration, read from the environment.
//!
//! Every value has a default that works for local development, so `Config::from_env`
//! never fails on a bare checkout. Malformed values are hard errors: silently falling
//! back to a default would hide a broken deployment.

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
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Address the server binds to. Defaults to `0.0.0.0:3000`.
    pub bind_addr: SocketAddr,
    /// Exact origins allowed by CORS. Credentials are enabled, so a wildcard is not
    /// legal per the Fetch spec; origins must be listed.
    pub cors_allowed_origins: Vec<String>,
    pub log_format: LogFormat,
    /// Prefix for RFC 7807 `type` URIs.
    pub problem_base_uri: String,
    /// Secret key for HMAC-SHA256 webhook signature validation.
    pub webhook_secret: Option<String>,
    /// Price list for COGS calculation (ury_daily_p_and_l.py:30).
    /// Defaults to "Buying". Not from stock valuation — buying prices are configured separately.
    pub buying_price_list: peacock_core::ids::PriceListName,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 3000)),
            cors_allowed_origins: vec![
                "http://localhost:5173".to_string(),
                "http://localhost:3000".to_string(),
            ],
            log_format: LogFormat::Pretty,
            problem_base_uri: "https://peacock-pos.example.com/errors".to_string(),
            webhook_secret: None,
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

        let webhook_secret = get(env_keys::WEBHOOK_SECRET)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let buying_price_list = defaults.buying_price_list;

        Ok(Self {
            bind_addr: SocketAddr::new(host, port),
            cors_allowed_origins,
            log_format,
            problem_base_uri,
            webhook_secret,
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

    #[test]
    fn defaults_bind_all_interfaces_on_3000() {
        let cfg = Config::from_source(source(&[])).expect("defaults are valid");
        assert_eq!(cfg.bind_addr.to_string(), "0.0.0.0:3000");
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
        let port = Config::from_source(source(&[(env_keys::PORT, "not-a-port")]));
        assert!(port.unwrap_err().contains(env_keys::PORT));

        let host = Config::from_source(source(&[(env_keys::HOST, "nope")]));
        assert!(host.unwrap_err().contains(env_keys::HOST));

        let format = Config::from_source(source(&[(env_keys::LOG_FORMAT, "yaml")]));
        assert!(format.unwrap_err().contains("invalid log format"));

        let origins = Config::from_source(source(&[(env_keys::CORS_ORIGINS, " , ")]));
        assert!(origins.unwrap_err().contains("empty"));
    }
}
