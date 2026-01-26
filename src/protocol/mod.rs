//! Protocol module - Defines the wire protocol for CoreNet communication
//!
//! The protocol uses a simple binary format for efficiency:
//! - 1 byte message type
//! - 4 bytes payload length (big-endian)
//! - 4 bytes sequence number (big-endian)
//! - Variable length payload

mod message;
mod codec;

pub use message::*;
pub use codec::*;

/// Protocol version for compatibility checking
pub const PROTOCOL_VERSION: u32 = 1;

/// Default port for CoreNet communication
pub const DEFAULT_PORT: u16 = 24800;

/// Magic bytes for protocol identification
pub const MAGIC_BYTES: [u8; 4] = [0x43, 0x4E, 0x45, 0x54]; // "CNET"
