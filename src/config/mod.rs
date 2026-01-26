//! Configuration module
//!
//! Handles loading and saving CoreNet configuration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::protocol::{ScreenEdge, DEFAULT_PORT};

/// Configuration errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Parse error: {0}")]
    Parse(#[from] toml::de::Error),
    
    #[error("Serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    
    #[error("Config file not found: {0}")]
    NotFound(PathBuf),
}

pub type ConfigResult<T> = Result<T, ConfigError>;

/// Main application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// General settings
    #[serde(default)]
    pub general: GeneralConfig,
    
    /// Screen settings
    #[serde(default)]
    pub screen: ScreenConfig,
    
    /// Network settings
    #[serde(default)]
    pub network: NetworkConfig,
    
    /// Neighbor configuration
    #[serde(default)]
    pub neighbors: HashMap<String, String>,
    
    /// Security settings
    #[serde(default)]
    pub security: SecurityConfig,
    
    /// Clipboard settings
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    
    /// Input settings
    #[serde(default)]
    pub input: InputConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            screen: ScreenConfig::default(),
            network: NetworkConfig::default(),
            neighbors: HashMap::new(),
            security: SecurityConfig::default(),
            clipboard: ClipboardConfig::default(),
            input: InputConfig::default(),
        }
    }
}

/// General configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Unique host identifier (auto-generated if not set)
    pub host_id: Option<String>,
    /// Human-readable name for this host
    pub name: String,
    /// Enable verbose logging
    #[serde(default)]
    pub verbose: bool,
    /// Log file path (optional)
    pub log_file: Option<PathBuf>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            host_id: None,
            name: hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            verbose: false,
            log_file: None,
        }
    }
}

/// Screen configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenConfig {
    /// Screen width override (auto-detected if not set)
    pub width: Option<u32>,
    /// Screen height override (auto-detected if not set)
    pub height: Option<u32>,
    /// Edge detection margin in pixels
    #[serde(default = "default_edge_margin")]
    pub edge_margin: u32,
    /// Dwell time before transition (ms)
    #[serde(default)]
    pub dwell_time_ms: u64,
    /// Require double-tap at edge to transition
    #[serde(default)]
    pub require_double_tap: bool,
}

fn default_edge_margin() -> u32 {
    1
}

impl Default for ScreenConfig {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            edge_margin: default_edge_margin(),
            dwell_time_ms: 0,
            require_double_tap: false,
        }
    }
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Port to listen on
    #[serde(default = "default_port")]
    pub port: u16,
    /// Interface to bind to (default: all)
    pub bind_address: Option<String>,
    /// Connection timeout in ms
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_ms: u64,
    /// Heartbeat interval in ms
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_ms: u64,
    /// Enable mDNS discovery
    #[serde(default = "default_true")]
    pub enable_discovery: bool,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_connect_timeout() -> u64 {
    5000
}

fn default_heartbeat_interval() -> u64 {
    1000
}

fn default_true() -> bool {
    true
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            bind_address: None,
            connect_timeout_ms: default_connect_timeout(),
            heartbeat_interval_ms: default_heartbeat_interval(),
            enable_discovery: default_true(),
        }
    }
}

/// Security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Require TLS for connections
    #[serde(default = "default_true")]
    pub require_tls: bool,
    /// Path to TLS certificate
    pub certificate: Option<PathBuf>,
    /// Path to TLS private key
    pub key: Option<PathBuf>,
    /// Allowed hosts (empty = allow all)
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            require_tls: false, // Disabled by default for easier setup
            certificate: None,
            key: None,
            allowed_hosts: Vec::new(),
        }
    }
}

/// Clipboard configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardConfig {
    /// Enable clipboard synchronization
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum clipboard size
    #[serde(default = "default_max_clipboard_size")]
    pub max_size_bytes: usize,
    /// Allowed MIME types (empty = allow all)
    #[serde(default)]
    pub allowed_types: Vec<String>,
}

fn default_max_clipboard_size() -> usize {
    10 * 1024 * 1024 // 10 MB
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_size_bytes: default_max_clipboard_size(),
            allowed_types: Vec::new(),
        }
    }
}

/// Input configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    /// Scroll multiplier
    #[serde(default = "default_scroll_multiplier")]
    pub scroll_multiplier: f32,
    /// Mouse acceleration
    #[serde(default = "default_mouse_acceleration")]
    pub mouse_acceleration: f32,
    /// Hotkey to toggle control (e.g., "ctrl+alt+space")
    pub toggle_hotkey: Option<String>,
    /// Hotkey to lock to current screen
    pub lock_hotkey: Option<String>,
}

fn default_scroll_multiplier() -> f32 {
    1.0
}

fn default_mouse_acceleration() -> f32 {
    1.0
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            scroll_multiplier: default_scroll_multiplier(),
            mouse_acceleration: default_mouse_acceleration(),
            toggle_hotkey: None,
            lock_hotkey: None,
        }
    }
}

impl Config {
    /// Load configuration from a file
    pub fn load(path: &Path) -> ConfigResult<Self> {
        if !path.exists() {
            return Err(ConfigError::NotFound(path.to_path_buf()));
        }
        
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Load configuration from the default location
    pub fn load_default() -> ConfigResult<Self> {
        let config_paths = [
            dirs::config_dir().map(|p| p.join("corenet/config.toml")),
            Some(PathBuf::from("./corenet.toml")),
            Some(PathBuf::from("./config.toml")),
        ];

        for path in config_paths.iter().flatten() {
            if path.exists() {
                return Self::load(path);
            }
        }

        // Return default config if no file found
        Ok(Self::default())
    }

    /// Save configuration to a file
    pub fn save(&self, path: &Path) -> ConfigResult<()> {
        let contents = toml::to_string_pretty(self)?;
        
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// Get the host ID, generating one if not set
    pub fn host_id(&self) -> String {
        self.general.host_id.clone().unwrap_or_else(|| {
            uuid::Uuid::new_v4().to_string()
        })
    }

    /// Get neighbor for a specific edge
    pub fn get_neighbor(&self, edge: ScreenEdge) -> Option<&String> {
        let edge_name = match edge {
            ScreenEdge::Left => "left",
            ScreenEdge::Right => "right",
            ScreenEdge::Top => "top",
            ScreenEdge::Bottom => "bottom",
        };
        self.neighbors.get(edge_name)
    }

    /// Set neighbor for a specific edge
    pub fn set_neighbor(&mut self, edge: ScreenEdge, host: String) {
        let edge_name = match edge {
            ScreenEdge::Left => "left",
            ScreenEdge::Right => "right",
            ScreenEdge::Top => "top",
            ScreenEdge::Bottom => "bottom",
        };
        self.neighbors.insert(edge_name.to_string(), host);
    }
}

/// Generate a sample configuration file
pub fn generate_sample_config() -> String {
    let config = Config {
        general: GeneralConfig {
            host_id: Some("my-macbook-pro".to_string()),
            name: "MacBook Pro".to_string(),
            verbose: false,
            log_file: None,
        },
        neighbors: {
            let mut m = HashMap::new();
            m.insert("right".to_string(), "desktop-pc".to_string());
            m
        },
        ..Default::default()
    };

    toml::to_string_pretty(&config).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.network.port, DEFAULT_PORT);
    }

    #[test]
    fn test_save_and_load() {
        let config = Config::default();
        let mut file = NamedTempFile::new().unwrap();
        
        config.save(file.path()).unwrap();
        
        let loaded = Config::load(file.path()).unwrap();
        assert_eq!(loaded.network.port, config.network.port);
    }

    #[test]
    fn test_sample_config() {
        let sample = generate_sample_config();
        let parsed: Config = toml::from_str(&sample).unwrap();
        assert_eq!(parsed.general.name, "MacBook Pro");
    }
}
