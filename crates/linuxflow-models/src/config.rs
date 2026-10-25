//! Manager configuration (network, timeouts, offline). No hard-coded local value.

use std::time::Duration;

pub const ENV_OFFLINE_HF: &str = "HF_HUB_OFFLINE";
pub const ENV_OFFLINE: &str = "LINUXFLOW_OFFLINE";
pub const ENV_MODEL_URL: &str = "LINUXFLOW_MODEL_URL";
pub const ENV_HF_ENDPOINT: &str = "HF_ENDPOINT";

#[derive(Debug, Clone)]
pub struct Config {
    /// Offline mode: no network request.
    pub offline: bool,
    /// Replaces the origin of the official catalogue URLs (`LINUXFLOW_MODEL_URL`, otherwise `HF_ENDPOINT`).
    pub endpoint: Option<String>,
    /// Honours the system `HTTPS_PROXY` / `NO_PROXY`.
    pub system_proxy: bool,
    pub connect_timeout: Duration,
    /// Maximum delay without a received byte (headers or body) before the connection is considered lost.
    pub stall_timeout: Duration,
    /// Attempts per mirror.
    pub max_attempts: u32,
    /// Wait before the 2nd attempt; doubled at each failure, capped at `retry_max`.
    pub retry_base: Duration,
    pub retry_max: Duration,
    /// Disk space margin required in addition to the remaining size.
    pub disk_margin_bytes: u64,
    /// Minimum interval between two progress notifications.
    pub progress_interval: Duration,
    /// Network identifier: name and version of the application, no personal data.
    pub user_agent: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            offline: false,
            endpoint: None,
            system_proxy: true,
            connect_timeout: Duration::from_secs(15),
            stall_timeout: Duration::from_secs(30),
            max_attempts: 4,
            retry_base: Duration::from_secs(2),
            retry_max: Duration::from_secs(30),
            disk_margin_bytes: 128 * 1024 * 1024,
            progress_interval: Duration::from_millis(100),
            user_agent: format!("linuxflow-models/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> Config {
        Config::from_lookup(|k| std::env::var(k).ok())
    }

    /// Reads the variables through `lookup` (injectable for the tests).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Config {
        let offline = [ENV_OFFLINE_HF, ENV_OFFLINE]
            .iter()
            .any(|k| lookup(k).is_some_and(|v| truthy(&v)));
        let endpoint = [ENV_MODEL_URL, ENV_HF_ENDPOINT]
            .iter()
            .filter_map(|k| lookup(k))
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .find(|v| v.starts_with("https://") || v.starts_with("http://"));
        Config {
            offline,
            endpoint,
            ..Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Config {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_are_online_without_endpoint() {
        let c = cfg(&[]);
        assert!(!c.offline);
        assert!(c.endpoint.is_none());
    }

    #[test]
    fn either_variable_enables_offline() {
        assert!(cfg(&[("HF_HUB_OFFLINE", "1")]).offline);
        assert!(cfg(&[("LINUXFLOW_OFFLINE", "true")]).offline);
        assert!(!cfg(&[("LINUXFLOW_OFFLINE", "0")]).offline);
        assert!(!cfg(&[("HF_HUB_OFFLINE", "")]).offline);
    }

    #[test]
    fn endpoint_precedence_and_validation() {
        let c = cfg(&[
            ("LINUXFLOW_MODEL_URL", "https://a.test/"),
            ("HF_ENDPOINT", "https://b.test"),
        ]);
        assert_eq!(c.endpoint.as_deref(), Some("https://a.test"));
        assert_eq!(
            cfg(&[("HF_ENDPOINT", "https://b.test")])
                .endpoint
                .as_deref(),
            Some("https://b.test")
        );
        assert!(cfg(&[("HF_ENDPOINT", "file:///etc")]).endpoint.is_none());
    }

    #[test]
    fn user_agent_has_no_personal_data() {
        let ua = Config::default().user_agent;
        assert_eq!(
            ua,
            format!("linuxflow-models/{}", env!("CARGO_PKG_VERSION"))
        );
    }
}
