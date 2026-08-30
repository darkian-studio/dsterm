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

/// Relay transport settings. The transport itself is added incrementally; these
/// fields are accepted now so config files do not churn later.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct RelayConfig {
    pub server_url: String,
    pub host_id_file: Option<String>,
    pub heartbeat_secs: u64,
    pub reconnect_secs: Vec<u64>,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            server_url: "https://localhost:3000".to_string(),
            host_id_file: None,
            heartbeat_secs: 25,
            reconnect_secs: vec![1, 2, 5, 10, 30],
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct SecurityConfig {
    pub key_file: Option<String>,
    pub clients_file: Option<String>,
    pub unknown_clients: UnknownClientPolicy,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            key_file: None,
            clients_file: None,
            unknown_clients: UnknownClientPolicy::RequiresApproval,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownClientPolicy {
    AlwaysAllow,
    AlwaysReject,
    #[default]
    RequiresApproval,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct FilesystemConfig {
    pub enabled: bool,
    pub workspace_root: Option<String>,
    pub max_read_bytes: usize,
}

impl Default for FilesystemConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            workspace_root: None,
            max_read_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct PortsConfig {
    pub kill_enabled: bool,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct ProxyConfig {
    pub enabled: bool,
}

/// Top-level dsterm configuration.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct DstermConfig {
    /// Authoritative host home directory, resolved once at startup. Empty until
    /// `init_config` fills it from the OS (`HOME`/`USERPROFILE`, else cwd).
    pub home: String,
    pub terminal: TerminalConfig,
    pub bridges: BridgesConfig,
    pub relay: RelayConfig,
    pub security: SecurityConfig,
    pub filesystem: FilesystemConfig,
    pub proxy: ProxyConfig,
    pub ports: PortsConfig,
}

impl DstermConfig {
    /// Helper for FIX-065: typed home path (keeps `home` as String for serde compat)
    #[allow(dead_code)]
    pub fn home_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.home)
    }

    /// Centralize `--remote` override (FIX-062)
    pub fn apply_remote_flag(&mut self, remote: bool) {
        if remote {
            self.filesystem.enabled = true;
        }
    }

    /// Load configuration from a TOML file at the given path.
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: DstermConfig = toml::from_str(&content)?;
        Ok(config)
    }
}
