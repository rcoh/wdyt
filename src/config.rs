//! Configuration, loaded from `~/.config/wdyt/config.toml`.
//!
//! Every field has an environment-variable override so a one-off run can
//! change behaviour without editing the file.

use std::ffi::OsStr;
use std::net::IpAddr;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The default span of ports wdyt will bind, chosen because the user already
/// forwards 3000-3010 from the dev host.
const DEFAULT_PORT_LOW: u16 = 3000;
const DEFAULT_PORT_HIGH: u16 = 3010;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Slack (or any) incoming-webhook URL that notifications are POSTed to.
    /// When unset, wdyt prints the link instead of sending it, which keeps
    /// the tool usable before it is configured.
    pub webhook_url: Option<String>,

    /// Lowest port wdyt will use, inclusive.
    pub port_low: u16,

    /// Highest port wdyt will use, inclusive.
    pub port_high: u16,

    /// Address the daemon binds. Loopback by default: the ports are reached
    /// through SSH forwarding, so there is no reason to accept traffic from
    /// the network.
    pub bind: IpAddr,

    /// Host used when building the URL that goes into the notification. This
    /// is what the *user's* browser resolves, not what the daemon binds, so it
    /// stays `localhost` even though `bind` is an IP.
    pub public_host: String,

    /// The workflow variable filled when `webhook_url` is a Slack *workflow
    /// trigger* (`hooks.slack.com/triggers/…`) rather than a classic incoming
    /// webhook. Slack rejects the message inside the workflow if this does not
    /// match a variable the workflow declares. Ignored for Block Kit webhooks.
    pub webhook_field: String,

    /// Syntax-highlighting theme name, as understood by `two_face`.
    /// See `wdyt themes` for the list.
    pub theme: String,

    /// How long a session's content stays available before it is dropped.
    /// `None` keeps sessions for as long as the daemon runs.
    pub session_ttl_hours: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            webhook_url: None,
            port_low: DEFAULT_PORT_LOW,
            port_high: DEFAULT_PORT_HIGH,
            bind: IpAddr::from([127, 0, 0, 1]),
            public_host: "localhost".to_owned(),
            // What Slack's own workflow templates name the message variable.
            webhook_field: "content".to_owned(),
            theme: "Nord".to_owned(),
            session_ttl_hours: Some(24),
        }
    }
}

impl Config {
    /// Loads the config file, applies environment overrides, and validates the
    /// result. A missing file is not an error; it yields the defaults.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let mut config = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("parsing config at {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };

        config.apply_env()?;
        config.validate()?;
        Ok(config)
    }

    /// Environment overrides. These win over the file so that a single command
    /// can be redirected without a config edit.
    fn apply_env(&mut self) -> Result<()> {
        if let Ok(url) = std::env::var("WDYT_WEBHOOK_URL") {
            // An explicitly empty value disables the webhook rather than
            // setting it to the empty string.
            self.webhook_url = (!url.is_empty()).then_some(url);
        }
        if let Ok(value) = std::env::var("WDYT_PORTS") {
            let (low, high) = parse_port_range(&value)?;
            self.port_low = low;
            self.port_high = high;
        }
        if let Ok(value) = std::env::var("WDYT_THEME") {
            self.theme = value;
        }
        if let Ok(value) = std::env::var("WDYT_WEBHOOK_FIELD") {
            self.webhook_field = value;
        }
        if let Ok(value) = std::env::var("WDYT_PUBLIC_HOST") {
            self.public_host = value;
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.port_low <= self.port_high,
            "port_low ({}) must not exceed port_high ({})",
            self.port_low,
            self.port_high
        );
        anyhow::ensure!(self.port_low != 0, "port_low must not be 0");
        if let Some(url) = &self.webhook_url {
            anyhow::ensure!(
                url.starts_with("http://") || url.starts_with("https://"),
                "webhook_url must be an http(s) URL"
            );
        }
        Ok(())
    }

    pub fn ports(&self) -> RangeInclusive<u16> {
        self.port_low..=self.port_high
    }

    /// The port the daemon itself listens on: the first of the range. Demos
    /// get the ports above it.
    pub fn daemon_port(&self) -> u16 {
        self.port_low
    }

    pub fn config_dir() -> Result<PathBuf> {
        let dirs = directories::BaseDirs::new().context("could not determine a home directory")?;
        Ok(dirs.config_dir().join("wdyt"))
    }

    pub fn state_dir() -> Result<PathBuf> {
        let dirs = directories::BaseDirs::new().context("could not determine a home directory")?;
        // XDG_STATE_HOME is the right home on Linux. Platforms without a
        // dedicated state directory fall back to their local data directory.
        let base = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
        Ok(base.join("wdyt"))
    }

    pub fn state_path() -> Result<PathBuf> {
        resolve_state_path(
            &Self::state_dir()?,
            std::env::var_os("WDYT_STATE_PATH").as_deref(),
        )
    }

    pub fn path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    /// Writes the config, creating the directory if needed.
    pub fn save(&self) -> Result<PathBuf> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

fn resolve_state_path(default_dir: &Path, override_path: Option<&OsStr>) -> Result<PathBuf> {
    match override_path {
        Some(path) => {
            anyhow::ensure!(!path.is_empty(), "WDYT_STATE_PATH must not be empty");
            Ok(PathBuf::from(path))
        }
        None => Ok(default_dir.join("sessions.json")),
    }
}

/// Parses `"3000-3010"` or a single `"3000"` into an inclusive range.
fn parse_port_range(value: &str) -> Result<(u16, u16)> {
    let value = value.trim();
    match value.split_once('-') {
        Some((low, high)) => {
            let low: u16 = low.trim().parse().context("invalid low port")?;
            let high: u16 = high.trim().parse().context("invalid high port")?;
            anyhow::ensure!(low <= high, "port range {value:?} is inverted");
            Ok((low, high))
        }
        None => {
            let port: u16 = value.parse().context("invalid port")?;
            Ok((port, port))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_range_and_single_port() {
        assert_eq!(parse_port_range("3000-3010").unwrap(), (3000, 3010));
        assert_eq!(parse_port_range(" 3005 ").unwrap(), (3005, 3005));
        assert!(parse_port_range("3010-3000").is_err());
        assert!(parse_port_range("nope").is_err());
    }

    #[test]
    fn rejects_inverted_and_zero_ports() {
        let mut config = Config {
            port_low: 4000,
            port_high: 3000,
            ..Config::default()
        };
        assert!(config.validate().is_err());

        config.port_low = 0;
        config.port_high = 10;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_non_http_webhook() {
        let config = Config {
            webhook_url: Some("ftp://example.com".to_owned()),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let config = Config::load_from(Path::new("/nonexistent/wdyt/config.toml")).unwrap();
        assert_eq!(config.port_low, DEFAULT_PORT_LOW);
        assert_eq!(config.port_high, DEFAULT_PORT_HIGH);
    }

    #[test]
    fn state_path_can_be_overridden_for_an_isolated_daemon() {
        let default = Path::new("/home/test/.local/state/wdyt");
        assert_eq!(
            resolve_state_path(default, None).unwrap(),
            default.join("sessions.json")
        );
        assert_eq!(
            resolve_state_path(default, Some(OsStr::new("/tmp/wdyt-test/state.json"))).unwrap(),
            PathBuf::from("/tmp/wdyt-test/state.json")
        );
        assert!(resolve_state_path(default, Some(OsStr::new(""))).is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.port_low, config.port_low);
        assert_eq!(parsed.theme, config.theme);
    }
}
