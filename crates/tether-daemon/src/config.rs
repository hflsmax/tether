use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: String,
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default)]
    pub socket_path: String,
    #[serde(default = "default_raw_log_size")]
    pub raw_log_size: usize,
    #[serde(default = "default_default_shell")]
    pub default_shell: String,
}

fn default_idle_timeout() -> String { "24h".into() }
fn default_scrollback_lines() -> usize { 10_000 }
fn default_max_sessions() -> usize { 20 }
fn default_raw_log_size() -> usize { 1024 * 1024 } // 1 MiB
fn default_default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout: default_idle_timeout(),
            scrollback_lines: default_scrollback_lines(),
            max_sessions: default_max_sessions(),
            socket_path: String::new(),
            raw_log_size: default_raw_log_size(),
            default_shell: default_default_shell(),
        }
    }
}

impl Config {
    pub fn socket_path(&self) -> PathBuf {
        if self.socket_path.is_empty() {
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
            PathBuf::from(runtime_dir).join("tether.sock")
        } else {
            PathBuf::from(&self.socket_path)
        }
    }

    pub fn idle_timeout_duration(&self) -> Duration {
        parse_duration(&self.idle_timeout).unwrap_or(Duration::from_secs(24 * 3600))
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}

fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(hours) = s.strip_suffix('h') {
        hours.parse::<u64>().ok().map(|h| Duration::from_secs(h * 3600))
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<u64>().ok().map(|m| Duration::from_secs(m * 60))
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.parse::<u64>().ok().map(Duration::from_secs)
    } else {
        s.parse::<u64>().ok().map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_durations() {
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(86400)));
        assert_eq!(parse_duration("30m"), Some(Duration::from_secs(1800)));
        assert_eq!(parse_duration("60s"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("3600"), Some(Duration::from_secs(3600)));
    }
}
