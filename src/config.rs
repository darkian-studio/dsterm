//! Runtime configuration loaded from an optional TOML file.
//!
//! If no config file is supplied, all fields fall back to their hard-coded
//! defaults (identical to the values previously baked into the source).
use serde::Deserialize;

/// Terminal PTY and WebSocket tuning.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TerminalConfig {
    /// Maximum bytes kept in the per-session scrollback file (default 256 KB).
    pub max_scrollback_bytes: usize,
    /// WebSocket output coalescing interval in milliseconds (default 8 ms).
    pub output_coalesce_ms: u64,
    /// PTY read buffer size in bytes; also used as the coalesce flush trigger (default 8 KB).
    pub read_buffer_bytes: usize,
    /// Seconds of inactivity before a terminal session is evicted (default 1800 = 30 min).
    pub inactivity_timeout_secs: u64,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            max_scrollback_bytes: 262_144,
            output_coalesce_ms: 8,
            read_buffer_bytes: 8_192,
            inactivity_timeout_secs: 1_800,
        }
    }
}

/// Bridge process lifecycle tuning (LSP, DAP, MCP, Extension Host).
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct BridgesConfig {
    /// Seconds to wait for a bridge process to exit before force-killing it (default 2).
    pub kill_timeout_secs: u64,
}

impl Default for BridgesConfig {
    fn default() -> Self {
        Self {
            kill_timeout_secs: 2,
        }
    }
}

/// Top-level dsterm configuration.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct DstermConfig {
    pub terminal: TerminalConfig,
    pub bridges: BridgesConfig,
}

impl DstermConfig {
    /// Load configuration from a TOML file at the given path.
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: DstermConfig = toml::from_str(&content)?;
        Ok(config)
    }
}
