//! Protocol codec for encoding/decoding messages
//!
//! Handles serialization and framing of protocol messages.

use bytes::{Buf, BufMut, BytesMut};
use std::io;
use thiserror::Error;

use super::{Message, MAGIC_BYTES};

/// Maximum message size (10 MB)
const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

/// Header size: magic(4) + type(1) + length(4) + sequence(4) = 13 bytes
const HEADER_SIZE: usize = 13;

/// Codec errors
#[derive(Error, Debug)]
pub enum CodecError {
    #[error("Invalid magic bytes")]
    InvalidMagic,
    
    #[error("Message too large: {0} bytes (max: {1})")]
    MessageTooLarge(usize, usize),
    
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    
    #[error("Incomplete message")]
    Incomplete,
}

/// Message frame with metadata
#[derive(Debug, Clone)]
pub struct Frame {
    /// Sequence number for ordering
    pub sequence: u32,
    /// The actual message
    pub message: Message,
}

impl Frame {
    pub fn new(sequence: u32, message: Message) -> Self {
        Self { sequence, message }
    }
}

/// Encodes messages into the wire format
pub struct Encoder {
    sequence: u32,
}

impl Encoder {
    pub fn new() -> Self {
        Self { sequence: 0 }
    }

    /// Encode a message into a buffer
    pub fn encode(&mut self, message: &Message, buf: &mut BytesMut) -> Result<(), CodecError> {
        // Serialize the message payload
        let payload = bincode::serialize(message)?;
        
        if payload.len() > MAX_MESSAGE_SIZE {
            return Err(CodecError::MessageTooLarge(payload.len(), MAX_MESSAGE_SIZE));
        }

        // Write header
        buf.put_slice(&MAGIC_BYTES);
        buf.put_u8(message.type_id());
        buf.put_u32(payload.len() as u32);
        buf.put_u32(self.sequence);
        
        // Write payload
        buf.put_slice(&payload);
        
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }

    /// Get a Frame with the current sequence number
    pub fn create_frame(&mut self, message: Message) -> Frame {
        let frame = Frame::new(self.sequence, message);
        self.sequence = self.sequence.wrapping_add(1);
        frame
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Decodes messages from the wire format
pub struct Decoder {
    state: DecodeState,
}

#[derive(Default)]
enum DecodeState {
    #[default]
    Header,
    Payload {
        message_type: u8,
        length: usize,
        sequence: u32,
    },
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            state: DecodeState::Header,
        }
    }

    /// Attempt to decode a frame from the buffer
    /// Returns Ok(None) if more data is needed
    pub fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Frame>, CodecError> {
        loop {
            match &self.state {
                DecodeState::Header => {
                    if buf.len() < HEADER_SIZE {
                        return Ok(None);
                    }

                    // Check magic bytes
                    if &buf[0..4] != MAGIC_BYTES {
                        return Err(CodecError::InvalidMagic);
                    }

                    let message_type = buf[4];
                    let length = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
                    let sequence = u32::from_be_bytes([buf[9], buf[10], buf[11], buf[12]]);

                    if length > MAX_MESSAGE_SIZE {
                        return Err(CodecError::MessageTooLarge(length, MAX_MESSAGE_SIZE));
                    }

                    buf.advance(HEADER_SIZE);
                    
                    self.state = DecodeState::Payload {
                        message_type,
                        length,
                        sequence,
                    };
                }
                DecodeState::Payload { message_type: _, length, sequence } => {
                    if buf.len() < *length {
                        return Ok(None);
                    }

                    let payload = buf.split_to(*length);
                    let message: Message = bincode::deserialize(&payload)?;
                    let seq = *sequence;
                    
                    self.state = DecodeState::Header;
                    
                    return Ok(Some(Frame::new(seq, message)));
                }
            }
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let mut encoder = Encoder::new();
        let mut decoder = Decoder::new();
        let mut buf = BytesMut::new();

        let original = Message::MouseMoveRelative { dx: 100, dy: -50 };
        encoder.encode(&original, &mut buf).unwrap();

        let frame = decoder.decode(&mut buf).unwrap().unwrap();
        
        match frame.message {
            Message::MouseMoveRelative { dx, dy } => {
                assert_eq!(dx, 100);
                assert_eq!(dy, -50);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_multiple_messages() {
        let mut encoder = Encoder::new();
        let mut decoder = Decoder::new();
        let mut buf = BytesMut::new();

        let messages = vec![
            Message::Heartbeat { timestamp: 12345 },
            Message::MouseButton { button: super::super::MouseButton::Left, pressed: true },
            Message::KeyDown { keycode: 0x04, character: Some('a'), modifiers: Default::default() },
        ];

        for msg in &messages {
            encoder.encode(msg, &mut buf).unwrap();
        }

        for (i, _original) in messages.iter().enumerate() {
            let frame = decoder.decode(&mut buf).unwrap().unwrap();
            assert_eq!(frame.sequence, i as u32);
        }
    }
}
