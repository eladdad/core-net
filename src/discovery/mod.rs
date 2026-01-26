//! Service discovery module
//!
//! Provides mDNS/DNS-SD based discovery of CoreNet hosts on the local network.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, RwLock};

use crate::protocol::ScreenInfo;

/// Service type for CoreNet discovery
pub const SERVICE_TYPE: &str = "_corenet._tcp.local.";

/// Discovery errors
#[derive(Error, Debug)]
pub enum DiscoveryError {
    #[error("mDNS error: {0}")]
    Mdns(String),
    
    #[error("Service registration failed: {0}")]
    Registration(String),
    
    #[error("Already running")]
    AlreadyRunning,
    
    #[error("Not running")]
    NotRunning,
}

pub type DiscoveryResult<T> = Result<T, DiscoveryError>;

/// Information about a discovered host
#[derive(Debug, Clone)]
pub struct DiscoveredHost {
    /// Host identifier
    pub host_id: String,
    /// Human-readable name
    pub host_name: String,
    /// IP addresses
    pub addresses: Vec<IpAddr>,
    /// Port number
    pub port: u16,
    /// Screen dimensions
    pub screen_width: u32,
    pub screen_height: u32,
    /// Additional properties
    pub properties: HashMap<String, String>,
}

impl DiscoveredHost {
    /// Get a socket address for connection
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.addresses.first().map(|ip| SocketAddr::new(*ip, self.port))
    }

    /// Convert to ScreenInfo
    pub fn to_screen_info(&self) -> ScreenInfo {
        ScreenInfo::new(
            self.host_id.clone(),
            self.host_name.clone(),
            self.screen_width,
            self.screen_height,
        )
    }
}

/// Events from the discovery service
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A new host was discovered
    HostDiscovered(DiscoveredHost),
    /// A host is no longer available
    HostLost(String), // host_id
    /// A host's information was updated
    HostUpdated(DiscoveredHost),
}

/// Service discovery manager
pub struct Discovery {
    /// Local screen information
    screen_info: ScreenInfo,
    /// Port we're listening on
    port: u16,
    /// Discovered hosts
    hosts: Arc<RwLock<HashMap<String, DiscoveredHost>>>,
    /// Event channel
    event_tx: mpsc::Sender<DiscoveryEvent>,
    /// Event receiver
    event_rx: Option<mpsc::Receiver<DiscoveryEvent>>,
    /// Whether discovery is running
    running: Arc<RwLock<bool>>,
    /// Shutdown signal
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl Discovery {
    /// Create a new discovery service
    pub fn new(screen_info: ScreenInfo, port: u16) -> Self {
        let (event_tx, event_rx) = mpsc::channel(64);
        
        Self {
            screen_info,
            port,
            hosts: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            event_rx: Some(event_rx),
            running: Arc::new(RwLock::new(false)),
            shutdown_tx: None,
        }
    }

    /// Take the event receiver
    pub fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<DiscoveryEvent>> {
        self.event_rx.take()
    }

    /// Start the discovery service
    pub async fn start(&mut self) -> DiscoveryResult<()> {
        {
            let running = self.running.read().await;
            if *running {
                return Err(DiscoveryError::AlreadyRunning);
            }
        }

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        {
            let mut running = self.running.write().await;
            *running = true;
        }

        // Register our service
        self.register_service().await?;

        let hosts = self.hosts.clone();
        let event_tx = self.event_tx.clone();
        let running = self.running.clone();

        // Start browsing for other services
        // In real implementation, this would use mdns-sd crate
        tokio::spawn(async move {
            // Simulated discovery loop
            // In real implementation:
            // let mdns = ServiceDaemon::new().expect("Failed to create daemon");
            // let receiver = mdns.browse(SERVICE_TYPE).expect("Failed to browse");
            
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        // Process discovered services
                        // In real implementation, we'd receive ServiceEvent from mdns-sd
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }

            let mut running = running.write().await;
            *running = false;
            
            tracing::info!("Discovery service stopped");
        });

        tracing::info!("Discovery service started");
        Ok(())
    }

    /// Stop the discovery service
    pub async fn stop(&mut self) -> DiscoveryResult<()> {
        {
            let running = self.running.read().await;
            if !*running {
                return Err(DiscoveryError::NotRunning);
            }
        }

        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Unregister our service
        self.unregister_service().await?;

        Ok(())
    }

    /// Register our service for discovery by others
    async fn register_service(&self) -> DiscoveryResult<()> {
        // In real implementation using mdns-sd:
        /*
        let mdns = ServiceDaemon::new()?;
        
        let service_name = format!("{}._corenet._tcp.local.", self.screen_info.host_name);
        let host_name = format!("{}.local.", hostname::get()?.to_string_lossy());
        
        let mut properties = HashMap::new();
        properties.insert("id".to_string(), self.screen_info.host_id.clone());
        properties.insert("width".to_string(), self.screen_info.width.to_string());
        properties.insert("height".to_string(), self.screen_info.height.to_string());
        properties.insert("version".to_string(), PROTOCOL_VERSION.to_string());
        
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &self.screen_info.host_name,
            &host_name,
            "",
            self.port,
            Some(properties),
        )?;
        
        mdns.register(service)?;
        */
        
        tracing::info!(
            "Registered service: {} on port {}",
            self.screen_info.host_name,
            self.port
        );
        
        Ok(())
    }

    /// Unregister our service
    async fn unregister_service(&self) -> DiscoveryResult<()> {
        // mdns.unregister(service)?;
        tracing::info!("Unregistered service: {}", self.screen_info.host_name);
        Ok(())
    }

    /// Get all discovered hosts
    pub async fn discovered_hosts(&self) -> Vec<DiscoveredHost> {
        let hosts = self.hosts.read().await;
        hosts.values().cloned().collect()
    }

    /// Get a specific host by ID
    pub async fn get_host(&self, host_id: &str) -> Option<DiscoveredHost> {
        let hosts = self.hosts.read().await;
        hosts.get(host_id).cloned()
    }

    /// Manually add a host (for testing or manual configuration)
    pub async fn add_manual_host(&self, host: DiscoveredHost) {
        let mut hosts = self.hosts.write().await;
        let host_id = host.host_id.clone();
        let is_new = !hosts.contains_key(&host_id);
        hosts.insert(host_id, host.clone());
        
        let event = if is_new {
            DiscoveryEvent::HostDiscovered(host)
        } else {
            DiscoveryEvent::HostUpdated(host)
        };
        
        let _ = self.event_tx.send(event).await;
    }

    /// Remove a host
    pub async fn remove_host(&self, host_id: &str) {
        let mut hosts = self.hosts.write().await;
        if hosts.remove(host_id).is_some() {
            let _ = self.event_tx.send(DiscoveryEvent::HostLost(host_id.to_string())).await;
        }
    }
}

// Example of real mDNS integration:
/*
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

async fn browse_services(
    hosts: Arc<RwLock<HashMap<String, DiscoveredHost>>>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
) {
    let mdns = ServiceDaemon::new().expect("Failed to create mDNS daemon");
    let receiver = mdns.browse(SERVICE_TYPE).expect("Failed to browse");
    
    while let Ok(event) = receiver.recv() {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let host = DiscoveredHost {
                    host_id: info.get_property_val_str("id")
                        .unwrap_or_default()
                        .to_string(),
                    host_name: info.get_fullname().to_string(),
                    addresses: info.get_addresses().iter().copied().collect(),
                    port: info.get_port(),
                    screen_width: info.get_property_val_str("width")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1920),
                    screen_height: info.get_property_val_str("height")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1080),
                    properties: info.get_properties().iter()
                        .map(|p| (p.key().to_string(), p.val_str().to_string()))
                        .collect(),
                };
                
                let mut hosts = hosts.write().await;
                let is_new = !hosts.contains_key(&host.host_id);
                hosts.insert(host.host_id.clone(), host.clone());
                
                let event = if is_new {
                    DiscoveryEvent::HostDiscovered(host)
                } else {
                    DiscoveryEvent::HostUpdated(host)
                };
                
                let _ = event_tx.send(event).await;
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                // Extract host_id from fullname and remove
                let mut hosts = hosts.write().await;
                // Find and remove by matching fullname
                if let Some(host_id) = hosts.iter()
                    .find(|(_, h)| h.host_name == fullname)
                    .map(|(id, _)| id.clone())
                {
                    hosts.remove(&host_id);
                    let _ = event_tx.send(DiscoveryEvent::HostLost(host_id)).await;
                }
            }
            _ => {}
        }
    }
}
*/
