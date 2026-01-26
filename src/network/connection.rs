//! Connection handling for CoreNet
//!
//! Manages individual peer connections, including:
//! - Message encoding/decoding
//! - Heartbeat handling
//! - Connection state management

use bytes::BytesMut;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};

use crate::protocol::{Decoder, Encoder, Frame, Message, ScreenInfo, PROTOCOL_VERSION};

/// Connection errors
#[derive(Error, Debug)]
pub enum ConnectionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Protocol error: {0}")]
    Protocol(#[from] crate::protocol::CodecError),
    
    #[error("Connection closed")]
    Closed,
    
    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),
    
    #[error("Protocol version mismatch: local={local}, remote={remote}")]
    VersionMismatch { local: u32, remote: u32 },
    
    #[error("Connection timeout")]
    Timeout,
    
    #[error("Send channel closed")]
    SendChannelClosed,
}

pub type ConnectionResult<T> = Result<T, ConnectionError>;

/// State of a connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state, not yet connected
    Disconnected,
    /// TCP connection established, awaiting handshake
    Connecting,
    /// Handshake complete, ready for communication
    Connected,
    /// Connection is closing gracefully
    Closing,
    /// Connection has been closed
    Closed,
}

/// Represents a connection to a remote CoreNet host
pub struct Connection {
    /// Remote peer address
    remote_addr: SocketAddr,
    /// The TCP stream
    stream: TcpStream,
    /// Protocol encoder
    encoder: Encoder,
    /// Protocol decoder
    decoder: Decoder,
    /// Read buffer
    read_buf: BytesMut,
    /// Write buffer
    write_buf: BytesMut,
    /// Remote screen info (populated after handshake)
    remote_screen_info: Option<ScreenInfo>,
    /// Connection state
    state: ConnectionState,
    /// Last activity timestamp
    last_activity: Instant,
    /// Statistics
    stats: ConnectionStats,
}

/// Connection statistics
#[derive(Debug, Default, Clone)]
pub struct ConnectionStats {
    /// Messages sent
    pub messages_sent: u64,
    /// Messages received
    pub messages_received: u64,
    /// Bytes sent
    pub bytes_sent: u64,
    /// Bytes received
    pub bytes_received: u64,
    /// Round-trip time (microseconds)
    pub rtt_us: u64,
}

impl Connection {
    /// Create a new connection from an established TCP stream
    pub fn new(stream: TcpStream, remote_addr: SocketAddr) -> Self {
        Self {
            remote_addr,
            stream,
            encoder: Encoder::new(),
            decoder: Decoder::new(),
            read_buf: BytesMut::with_capacity(4096),
            write_buf: BytesMut::with_capacity(4096),
            remote_screen_info: None,
            state: ConnectionState::Connecting,
            last_activity: Instant::now(),
            stats: ConnectionStats::default(),
        }
    }

    /// Get the remote address
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    /// Get the current connection state
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Get the remote screen info (if handshake completed)
    pub fn remote_screen_info(&self) -> Option<&ScreenInfo> {
        self.remote_screen_info.as_ref()
    }

    /// Get connection statistics
    pub fn stats(&self) -> &ConnectionStats {
        &self.stats
    }

    /// Perform the server-side handshake
    pub async fn handshake_server(&mut self, local_screen: &ScreenInfo) -> ConnectionResult<()> {
        // Wait for Hello from client
        let frame = self.recv().await?.ok_or_else(|| {
            ConnectionError::HandshakeFailed("Connection closed during handshake".to_string())
        })?;

        let (remote_version, remote_screen) = match frame.message {
            Message::Hello { protocol_version, screen_info } => {
                (protocol_version, screen_info)
            }
            _ => {
                return Err(ConnectionError::HandshakeFailed(
                    "Expected Hello message".to_string(),
                ));
            }
        };

        // Check protocol version
        if remote_version != PROTOCOL_VERSION {
            // Send rejection
            self.send(&Message::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                screen_info: local_screen.clone(),
                accepted: false,
                reason: Some(format!(
                    "Protocol version mismatch: expected {}, got {}",
                    PROTOCOL_VERSION, remote_version
                )),
            })
            .await?;

            return Err(ConnectionError::VersionMismatch {
                local: PROTOCOL_VERSION,
                remote: remote_version,
            });
        }

        // Send acceptance
        self.send(&Message::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            screen_info: local_screen.clone(),
            accepted: true,
            reason: None,
        })
        .await?;

        self.remote_screen_info = Some(remote_screen);
        self.state = ConnectionState::Connected;
        
        tracing::info!(
            "Handshake complete with {} ({})",
            self.remote_screen_info.as_ref().unwrap().host_name,
            self.remote_addr
        );

        Ok(())
    }

    /// Perform the client-side handshake
    pub async fn handshake_client(&mut self, local_screen: &ScreenInfo) -> ConnectionResult<()> {
        // Send Hello
        self.send(&Message::Hello {
            protocol_version: PROTOCOL_VERSION,
            screen_info: local_screen.clone(),
        })
        .await?;

        // Wait for HelloAck
        let frame = self.recv().await?.ok_or_else(|| {
            ConnectionError::HandshakeFailed("Connection closed during handshake".to_string())
        })?;

        match frame.message {
            Message::HelloAck {
                protocol_version,
                screen_info,
                accepted,
                reason,
            } => {
                if !accepted {
                    return Err(ConnectionError::HandshakeFailed(
                        reason.unwrap_or_else(|| "Connection rejected".to_string()),
                    ));
                }

                if protocol_version != PROTOCOL_VERSION {
                    return Err(ConnectionError::VersionMismatch {
                        local: PROTOCOL_VERSION,
                        remote: protocol_version,
                    });
                }

                self.remote_screen_info = Some(screen_info);
                self.state = ConnectionState::Connected;
                
                tracing::info!(
                    "Handshake complete with {} ({})",
                    self.remote_screen_info.as_ref().unwrap().host_name,
                    self.remote_addr
                );

                Ok(())
            }
            _ => Err(ConnectionError::HandshakeFailed(
                "Expected HelloAck message".to_string(),
            )),
        }
    }

    /// Send a message
    pub async fn send(&mut self, message: &Message) -> ConnectionResult<()> {
        self.write_buf.clear();
        self.encoder.encode(message, &mut self.write_buf)?;
        
        self.stream.write_all(&self.write_buf).await?;
        self.stream.flush().await?;
        
        self.stats.messages_sent += 1;
        self.stats.bytes_sent += self.write_buf.len() as u64;
        self.last_activity = Instant::now();
        
        Ok(())
    }

    /// Receive a message (returns None if no complete message available)
    pub async fn recv(&mut self) -> ConnectionResult<Option<Frame>> {
        loop {
            // Try to decode a message from the buffer
            if let Some(frame) = self.decoder.decode(&mut self.read_buf)? {
                self.stats.messages_received += 1;
                self.last_activity = Instant::now();
                return Ok(Some(frame));
            }

            // Read more data
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).await?;
            
            if n == 0 {
                if self.read_buf.is_empty() {
                    return Ok(None); // Clean close
                } else {
                    return Err(ConnectionError::Closed);
                }
            }
            
            self.read_buf.extend_from_slice(&buf[..n]);
            self.stats.bytes_received += n as u64;
        }
    }

    /// Try to receive a message with a timeout
    pub async fn recv_timeout(&mut self, timeout: Duration) -> ConnectionResult<Option<Frame>> {
        match tokio::time::timeout(timeout, self.recv()).await {
            Ok(result) => result,
            Err(_) => Err(ConnectionError::Timeout),
        }
    }

    /// Send a heartbeat and wait for response
    pub async fn ping(&mut self) -> ConnectionResult<Duration> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;

        let start = Instant::now();
        
        self.send(&Message::Heartbeat { timestamp }).await?;
        
        // Wait for HeartbeatAck
        let frame = self.recv_timeout(Duration::from_secs(5)).await?.ok_or(ConnectionError::Closed)?;
        
        match frame.message {
            Message::HeartbeatAck { timestamp: ts } if ts == timestamp => {
                let rtt = start.elapsed();
                self.stats.rtt_us = rtt.as_micros() as u64;
                Ok(rtt)
            }
            _ => Err(ConnectionError::Protocol(
                crate::protocol::CodecError::InvalidMagic,
            )),
        }
    }

    /// Close the connection gracefully
    pub async fn close(&mut self, reason: &str) -> ConnectionResult<()> {
        self.state = ConnectionState::Closing;
        
        self.send(&Message::Disconnect {
            reason: reason.to_string(),
        })
        .await?;
        
        self.stream.shutdown().await?;
        self.state = ConnectionState::Closed;
        
        Ok(())
    }

    /// Get the underlying stream for advanced operations
    pub fn stream_ref(&self) -> &TcpStream {
        &self.stream
    }

    /// Check if the connection is still active
    pub fn is_active(&self) -> bool {
        matches!(self.state, ConnectionState::Connected)
    }

    /// Get time since last activity
    pub fn idle_time(&self) -> Duration {
        self.last_activity.elapsed()
    }
}

/// A handle for sending messages to a connection
#[derive(Clone, Debug)]
pub struct ConnectionHandle {
    sender: mpsc::Sender<Message>,
    connected: Arc<AtomicBool>,
    rtt_us: Arc<AtomicU64>,
}

impl ConnectionHandle {
    pub fn new(sender: mpsc::Sender<Message>) -> Self {
        Self {
            sender,
            connected: Arc::new(AtomicBool::new(true)),
            rtt_us: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Send a message through this connection
    pub async fn send(&self, message: Message) -> Result<(), ConnectionError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(ConnectionError::Closed);
        }

        self.sender
            .send(message)
            .await
            .map_err(|_| ConnectionError::SendChannelClosed)
    }

    /// Check if the connection is still active
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Get the current round-trip time in microseconds
    pub fn rtt_us(&self) -> u64 {
        self.rtt_us.load(Ordering::SeqCst)
    }

    /// Mark the connection as disconnected
    pub fn mark_disconnected(&self) {
        self.connected.store(false, Ordering::SeqCst);
    }

    /// Update the RTT value
    pub fn update_rtt(&self, rtt_us: u64) {
        self.rtt_us.store(rtt_us, Ordering::SeqCst);
    }
}
