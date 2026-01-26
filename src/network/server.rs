//! CoreNet Server
//!
//! The server accepts connections from clients and coordinates
//! input sharing between connected hosts.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};

use super::connection::{Connection, ConnectionError, ConnectionHandle};
use super::NetworkConfig;
use crate::protocol::{Message, ScreenInfo};

/// Server errors
#[derive(Error, Debug)]
pub enum ServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Connection error: {0}")]
    Connection(#[from] ConnectionError),
    
    #[error("Server already running")]
    AlreadyRunning,
    
    #[error("Server not running")]
    NotRunning,
    
    #[error("Bind failed: {0}")]
    BindFailed(String),
}

pub type ServerResult<T> = Result<T, ServerError>;

/// Events emitted by the server
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// A new client has connected
    ClientConnected {
        addr: SocketAddr,
        screen_info: ScreenInfo,
    },
    /// A client has disconnected
    ClientDisconnected {
        addr: SocketAddr,
        reason: String,
    },
    /// Received a message from a client
    MessageReceived {
        addr: SocketAddr,
        message: Message,
    },
    /// Server started
    Started {
        bind_addr: SocketAddr,
    },
    /// Server stopped
    Stopped,
    /// Error occurred
    Error {
        message: String,
    },
}

/// Information about a connected client
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// Client address
    pub addr: SocketAddr,
    /// Client screen information
    pub screen_info: ScreenInfo,
    /// Handle for sending messages to this client
    pub handle: ConnectionHandle,
}

/// CoreNet Server
pub struct Server {
    /// Server configuration
    config: NetworkConfig,
    /// Local screen information
    screen_info: ScreenInfo,
    /// Connected clients
    clients: Arc<RwLock<HashMap<SocketAddr, ClientInfo>>>,
    /// Event sender
    event_tx: mpsc::Sender<ServerEvent>,
    /// Event receiver (for consumers)
    event_rx: Option<mpsc::Receiver<ServerEvent>>,
    /// Shutdown signal
    shutdown_tx: Option<mpsc::Sender<()>>,
    /// Whether the server is running
    running: Arc<RwLock<bool>>,
}

impl Server {
    /// Create a new server
    pub fn new(config: NetworkConfig, screen_info: ScreenInfo) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        
        Self {
            config,
            screen_info,
            clients: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            event_rx: Some(event_rx),
            shutdown_tx: None,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Take the event receiver (can only be called once)
    pub fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<ServerEvent>> {
        self.event_rx.take()
    }

    /// Start the server
    pub async fn start(&mut self) -> ServerResult<()> {
        {
            let running = self.running.read().await;
            if *running {
                return Err(ServerError::AlreadyRunning);
            }
        }

        let bind_addr = format!("0.0.0.0:{}", self.config.port);
        let listener = TcpListener::bind(&bind_addr).await.map_err(|e| {
            ServerError::BindFailed(format!("Failed to bind to {}: {}", bind_addr, e))
        })?;

        let local_addr = listener.local_addr()?;
        tracing::info!("Server listening on {}", local_addr);

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        {
            let mut running = self.running.write().await;
            *running = true;
        }

        let _ = self.event_tx.send(ServerEvent::Started { bind_addr: local_addr }).await;

        let clients = self.clients.clone();
        let event_tx = self.event_tx.clone();
        let screen_info = self.screen_info.clone();
        let running = self.running.clone();

        // Spawn the accept loop
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, addr)) => {
                                tracing::info!("New connection from {}", addr);
                                
                                let clients = clients.clone();
                                let event_tx = event_tx.clone();
                                let screen_info = screen_info.clone();
                                
                                tokio::spawn(async move {
                                    if let Err(e) = handle_client(
                                        stream,
                                        addr,
                                        clients,
                                        event_tx,
                                        screen_info,
                                    ).await {
                                        tracing::error!("Client handler error: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::error!("Accept error: {}", e);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        tracing::info!("Server shutdown requested");
                        break;
                    }
                }
            }

            let mut running = running.write().await;
            *running = false;
            
            let _ = event_tx.send(ServerEvent::Stopped).await;
        });

        Ok(())
    }

    /// Stop the server
    pub async fn stop(&mut self) -> ServerResult<()> {
        {
            let running = self.running.read().await;
            if !*running {
                return Err(ServerError::NotRunning);
            }
        }

        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Disconnect all clients
        let clients = self.clients.read().await;
        for (_, client) in clients.iter() {
            let _ = client.handle.send(Message::Disconnect {
                reason: "Server shutting down".to_string(),
            }).await;
        }

        Ok(())
    }

    /// Get a list of connected clients
    pub async fn clients(&self) -> Vec<ClientInfo> {
        let clients = self.clients.read().await;
        clients.values().cloned().collect()
    }

    /// Send a message to a specific client
    pub async fn send_to(&self, addr: &SocketAddr, message: Message) -> ServerResult<()> {
        let clients = self.clients.read().await;
        if let Some(client) = clients.get(addr) {
            client.handle.send(message).await?;
            Ok(())
        } else {
            Err(ServerError::Connection(ConnectionError::Closed))
        }
    }

    /// Send a message to all connected clients
    pub async fn broadcast(&self, message: Message) {
        let clients = self.clients.read().await;
        for (_, client) in clients.iter() {
            let _ = client.handle.send(message.clone()).await;
        }
    }

    /// Check if the server is running
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }
}

/// Handle a client connection
async fn handle_client(
    stream: TcpStream,
    addr: SocketAddr,
    clients: Arc<RwLock<HashMap<SocketAddr, ClientInfo>>>,
    event_tx: mpsc::Sender<ServerEvent>,
    screen_info: ScreenInfo,
) -> Result<(), ConnectionError> {
    let mut conn = Connection::new(stream, addr);
    
    // Perform handshake
    conn.handshake_server(&screen_info).await?;
    
    let remote_screen = conn.remote_screen_info().cloned().unwrap();
    
    // Create message channel for this client
    let (msg_tx, mut msg_rx) = mpsc::channel::<Message>(256);
    let handle = ConnectionHandle::new(msg_tx);
    
    // Store client info
    {
        let mut clients = clients.write().await;
        clients.insert(addr, ClientInfo {
            addr,
            screen_info: remote_screen.clone(),
            handle: handle.clone(),
        });
    }
    
    // Notify about new connection
    let _ = event_tx.send(ServerEvent::ClientConnected {
        addr,
        screen_info: remote_screen,
    }).await;
    
    // Main message loop
    let disconnect_reason = loop {
        tokio::select! {
            // Receive messages from the client
            result = conn.recv() => {
                match result {
                    Ok(Some(frame)) => {
                        match &frame.message {
                            Message::Disconnect { reason } => {
                                break reason.clone();
                            }
                            Message::Heartbeat { timestamp } => {
                                // Respond to heartbeat
                                let _ = conn.send(&Message::HeartbeatAck {
                                    timestamp: *timestamp,
                                }).await;
                            }
                            _ => {
                                // Forward message to event handler
                                let _ = event_tx.send(ServerEvent::MessageReceived {
                                    addr,
                                    message: frame.message,
                                }).await;
                            }
                        }
                    }
                    Ok(None) => {
                        break "Connection closed".to_string();
                    }
                    Err(e) => {
                        break format!("Error: {}", e);
                    }
                }
            }
            
            // Send messages to the client
            Some(message) = msg_rx.recv() => {
                if let Err(e) = conn.send(&message).await {
                    break format!("Send error: {}", e);
                }
            }
        }
    };
    
    // Clean up
    handle.mark_disconnected();
    
    {
        let mut clients = clients.write().await;
        clients.remove(&addr);
    }
    
    let _ = event_tx.send(ServerEvent::ClientDisconnected {
        addr,
        reason: disconnect_reason,
    }).await;
    
    let _ = conn.close("Session ended").await;
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_server_creation() {
        let config = NetworkConfig::default();
        let screen_info = ScreenInfo::new(
            "test-host".to_string(),
            "Test Host".to_string(),
            1920,
            1080,
        );
        
        let server = Server::new(config, screen_info);
        assert!(!server.is_running().await);
    }
}
