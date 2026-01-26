//! Network module - Handles TCP/TLS communication between hosts
//!
//! Provides:
//! - Server for accepting incoming connections
//! - Client for connecting to servers
//! - Connection management and message routing

mod server;
mod client;
mod connection;

pub use server::*;
pub use client::*;
pub use connection::*;

use std::net::SocketAddr;

/// Configuration for network operations
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Port to listen on or connect to
    pub port: u16,
    /// Whether to use TLS encryption
    pub use_tls: bool,
    /// Path to TLS certificate (for server)
    pub cert_path: Option<String>,
    /// Path to TLS private key (for server)
    pub key_path: Option<String>,
    /// Connection timeout in milliseconds
    pub connect_timeout_ms: u64,
    /// Heartbeat interval in milliseconds
    pub heartbeat_interval_ms: u64,
    /// Maximum message size
    pub max_message_size: usize,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            port: crate::protocol::DEFAULT_PORT,
            use_tls: true,
            cert_path: None,
            key_path: None,
            connect_timeout_ms: 5000,
            heartbeat_interval_ms: 1000,
            max_message_size: 10 * 1024 * 1024, // 10 MB
        }
    }
}

impl NetworkConfig {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            ..Default::default()
        }
    }

    pub fn with_tls(mut self, cert_path: String, key_path: String) -> Self {
        self.use_tls = true;
        self.cert_path = Some(cert_path);
        self.key_path = Some(key_path);
        self
    }

    pub fn without_tls(mut self) -> Self {
        self.use_tls = false;
        self
    }
}

/// Resolve a hostname to a socket address
pub async fn resolve_host(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    use tokio::net::lookup_host;
    
    let addr_string = format!("{}:{}", host, port);
    let mut addrs = lookup_host(&addr_string).await?;
    
    addrs.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Could not resolve host: {}", host),
        )
    })
}
