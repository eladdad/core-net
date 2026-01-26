//! CoreNet Client
//!
//! Connects to a CoreNet server and handles message exchange.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};

use super::connection::{Connection, ConnectionError, ConnectionHandle};
use super::NetworkConfig;
use crate::protocol::{Message, ScreenInfo};

/// Client errors
#[derive(Error, Debug)]
pub enum ClientError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Connection error: {0}")]
    Connection(#[from] ConnectionError),
    
    #[error("Already connected")]
    AlreadyConnected,
    
    #[error("Not connected")]
    NotConnected,
    
    #[error("Connection timeout")]
    Timeout,
}

pub type ClientResult<T> = Result<T, ClientError>;

/// Events emitted by the client
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Successfully connected to server
    Connected {
        server_addr: SocketAddr,
        server_screen: ScreenInfo,
    },
    /// Disconnected from server
    Disconnected {
        reason: String,
    },
    /// Received a message from the server
    MessageReceived {
        message: Message,
    },
    /// Connection error
    Error {
        message: String,
    },
}

/// Client state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    Disconnected,
    Connecting,
    Connected,
}

/// CoreNet Client
pub struct Client {
    /// Client configuration
    config: NetworkConfig,
    /// Local screen information
    screen_info: ScreenInfo,
    /// Current state
    state: Arc<RwLock<ClientState>>,
    /// Server screen info (after connection)
    server_screen: Arc<RwLock<Option<ScreenInfo>>>,
    /// Event sender
    event_tx: mpsc::Sender<ClientEvent>,
    /// Event receiver (for consumers)
    event_rx: Option<mpsc::Receiver<ClientEvent>>,
    /// Connection handle for sending messages
    connection_handle: Arc<RwLock<Option<ConnectionHandle>>>,
    /// Shutdown signal
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

impl Client {
    /// Create a new client
    pub fn new(config: NetworkConfig, screen_info: ScreenInfo) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        
        Self {
            config,
            screen_info,
            state: Arc::new(RwLock::new(ClientState::Disconnected)),
            server_screen: Arc::new(RwLock::new(None)),
            event_tx,
            event_rx: Some(event_rx),
            connection_handle: Arc::new(RwLock::new(None)),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    /// Take the event receiver (can only be called once)
    pub fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<ClientEvent>> {
        self.event_rx.take()
    }

    /// Connect to a server by address
    pub async fn connect(&self, server_addr: SocketAddr) -> ClientResult<()> {
        {
            let state = self.state.read().await;
            if *state != ClientState::Disconnected {
                return Err(ClientError::AlreadyConnected);
            }
        }

        {
            let mut state = self.state.write().await;
            *state = ClientState::Connecting;
        }

        tracing::info!("Connecting to {}", server_addr);

        // Connect with timeout
        let stream = match tokio::time::timeout(
            Duration::from_millis(self.config.connect_timeout_ms),
            TcpStream::connect(server_addr),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                let mut state = self.state.write().await;
                *state = ClientState::Disconnected;
                return Err(ClientError::Io(e));
            }
            Err(_) => {
                let mut state = self.state.write().await;
                *state = ClientState::Disconnected;
                return Err(ClientError::Timeout);
            }
        };

        let mut conn = Connection::new(stream, server_addr);
        
        // Perform handshake
        if let Err(e) = conn.handshake_client(&self.screen_info).await {
            let mut state = self.state.write().await;
            *state = ClientState::Disconnected;
            return Err(ClientError::Connection(e));
        }

        let server_screen = conn.remote_screen_info().cloned().unwrap();
        
        {
            let mut ss = self.server_screen.write().await;
            *ss = Some(server_screen.clone());
        }

        // Create message channel
        let (msg_tx, mut msg_rx) = mpsc::channel::<Message>(256);
        let handle = ConnectionHandle::new(msg_tx);

        {
            let mut ch = self.connection_handle.write().await;
            *ch = Some(handle.clone());
        }

        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        {
            let mut st = self.shutdown_tx.write().await;
            *st = Some(shutdown_tx);
        }

        {
            let mut state = self.state.write().await;
            *state = ClientState::Connected;
        }

        let _ = self.event_tx.send(ClientEvent::Connected {
            server_addr,
            server_screen,
        }).await;

        // Spawn the message loop
        let event_tx = self.event_tx.clone();
        let state = self.state.clone();
        let connection_handle = self.connection_handle.clone();
        let heartbeat_interval = Duration::from_millis(self.config.heartbeat_interval_ms);

        tokio::spawn(async move {
            let mut heartbeat_timer = tokio::time::interval(heartbeat_interval);
            
            let disconnect_reason = loop {
                tokio::select! {
                    // Receive messages from the server
                    result = conn.recv() => {
                        match result {
                            Ok(Some(frame)) => {
                                match &frame.message {
                                    Message::Disconnect { reason } => {
                                        break reason.clone();
                                    }
                                    Message::Heartbeat { timestamp } => {
                                        let _ = conn.send(&Message::HeartbeatAck {
                                            timestamp: *timestamp,
                                        }).await;
                                    }
                                    Message::HeartbeatAck { .. } => {
                                        // Update RTT
                                        handle.update_rtt(conn.stats().rtt_us);
                                    }
                                    _ => {
                                        let _ = event_tx.send(ClientEvent::MessageReceived {
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
                    
                    // Send messages to the server
                    Some(message) = msg_rx.recv() => {
                        if let Err(e) = conn.send(&message).await {
                            break format!("Send error: {}", e);
                        }
                    }
                    
                    // Send heartbeats
                    _ = heartbeat_timer.tick() => {
                        if let Err(e) = conn.send(&Message::Heartbeat {
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_micros() as u64,
                        }).await {
                            break format!("Heartbeat error: {}", e);
                        }
                    }
                    
                    // Shutdown signal
                    _ = shutdown_rx.recv() => {
                        break "Client shutdown requested".to_string();
                    }
                }
            };

            // Clean up
            handle.mark_disconnected();
            
            {
                let mut ch = connection_handle.write().await;
                *ch = None;
            }
            
            {
                let mut s = state.write().await;
                *s = ClientState::Disconnected;
            }
            
            let _ = conn.close(&disconnect_reason).await;
            
            let _ = event_tx.send(ClientEvent::Disconnected {
                reason: disconnect_reason,
            }).await;
        });

        Ok(())
    }

    /// Connect to a server by hostname
    pub async fn connect_hostname(&self, hostname: &str, port: u16) -> ClientResult<()> {
        let addr = super::resolve_host(hostname, port).await?;
        self.connect(addr).await
    }

    /// Disconnect from the server
    pub async fn disconnect(&self) -> ClientResult<()> {
        {
            let state = self.state.read().await;
            if *state == ClientState::Disconnected {
                return Err(ClientError::NotConnected);
            }
        }

        // Send disconnect message
        if let Some(handle) = &*self.connection_handle.read().await {
            let _ = handle.send(Message::Disconnect {
                reason: "Client disconnecting".to_string(),
            }).await;
        }

        // Signal shutdown
        if let Some(tx) = &*self.shutdown_tx.read().await {
            let _ = tx.send(()).await;
        }

        Ok(())
    }

    /// Send a message to the server
    pub async fn send(&self, message: Message) -> ClientResult<()> {
        let handle = self.connection_handle.read().await;
        if let Some(h) = &*handle {
            h.send(message).await?;
            Ok(())
        } else {
            Err(ClientError::NotConnected)
        }
    }

    /// Get the current state
    pub async fn state(&self) -> ClientState {
        *self.state.read().await
    }

    /// Get the server's screen info (if connected)
    pub async fn server_screen(&self) -> Option<ScreenInfo> {
        self.server_screen.read().await.clone()
    }

    /// Check if connected
    pub async fn is_connected(&self) -> bool {
        *self.state.read().await == ClientState::Connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        let config = NetworkConfig::default();
        let screen_info = ScreenInfo::new(
            "test-client".to_string(),
            "Test Client".to_string(),
            1920,
            1080,
        );
        
        let client = Client::new(config, screen_info);
        assert!(!client.is_connected().await);
    }
}
