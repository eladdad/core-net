//! Protocol message definitions
//!
//! Defines all message types used for communication between CoreNet hosts.

use serde::{Deserialize, Serialize};

/// Mouse button identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MouseButton {
    Left = 0,
    Right = 1,
    Middle = 2,
    Button4 = 3,
    Button5 = 4,
}

/// Keyboard modifier flags
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,  // Command on macOS, Windows key on Windows
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl Modifiers {
    pub fn to_bits(&self) -> u8 {
        let mut bits = 0u8;
        if self.shift { bits |= 0x01; }
        if self.ctrl { bits |= 0x02; }
        if self.alt { bits |= 0x04; }
        if self.meta { bits |= 0x08; }
        if self.caps_lock { bits |= 0x10; }
        if self.num_lock { bits |= 0x20; }
        bits
    }

    pub fn from_bits(bits: u8) -> Self {
        Self {
            shift: bits & 0x01 != 0,
            ctrl: bits & 0x02 != 0,
            alt: bits & 0x04 != 0,
            meta: bits & 0x08 != 0,
            caps_lock: bits & 0x10 != 0,
            num_lock: bits & 0x20 != 0,
        }
    }
}

/// Screen edge identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ScreenEdge {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

/// Screen information for a host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenInfo {
    /// Unique host identifier
    pub host_id: String,
    /// Human-readable host name
    pub host_name: String,
    /// Screen width in pixels
    pub width: u32,
    /// Screen height in pixels
    pub height: u32,
    /// Horizontal DPI
    pub dpi_x: f32,
    /// Vertical DPI
    pub dpi_y: f32,
}

impl ScreenInfo {
    pub fn new(host_id: String, host_name: String, width: u32, height: u32) -> Self {
        Self {
            host_id,
            host_name,
            width,
            height,
            dpi_x: 96.0,
            dpi_y: 96.0,
        }
    }
}

/// All possible protocol messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    /// Protocol handshake - sent on connection establishment
    Hello {
        protocol_version: u32,
        screen_info: ScreenInfo,
    },

    /// Acknowledgment of Hello
    HelloAck {
        protocol_version: u32,
        screen_info: ScreenInfo,
        accepted: bool,
        reason: Option<String>,
    },

    /// Relative mouse movement
    MouseMoveRelative {
        dx: i32,
        dy: i32,
    },

    /// Absolute mouse position
    MouseMoveAbsolute {
        x: i32,
        y: i32,
    },

    /// Mouse button press/release
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },

    /// Mouse scroll wheel
    MouseScroll {
        dx: i32,  // Horizontal scroll
        dy: i32,  // Vertical scroll
    },

    /// Keyboard key press
    KeyDown {
        /// Platform-independent key code (USB HID codes)
        keycode: u32,
        /// Unicode character if applicable
        character: Option<char>,
        /// Current modifier state
        modifiers: Modifiers,
    },

    /// Keyboard key release
    KeyUp {
        keycode: u32,
        modifiers: Modifiers,
    },

    /// Control has entered this screen from an edge
    EnterScreen {
        /// Which edge the cursor entered from
        edge: ScreenEdge,
        /// Position along the edge (0.0 to 1.0)
        position: f32,
    },

    /// Control is leaving this screen via an edge
    LeaveScreen {
        /// Which edge the cursor is leaving from
        edge: ScreenEdge,
        /// Position along the edge (0.0 to 1.0)
        position: f32,
    },

    /// Clipboard data synchronization
    ClipboardData {
        /// MIME type of the data
        mime_type: String,
        /// The actual clipboard content
        data: Vec<u8>,
    },

    /// Request clipboard data
    ClipboardRequest,

    /// Grab keyboard focus (for hotkey activation)
    GrabKeyboard,

    /// Release keyboard focus
    ReleaseKeyboard,

    /// Heartbeat to keep connection alive
    Heartbeat {
        timestamp: u64,
    },

    /// Response to heartbeat
    HeartbeatAck {
        timestamp: u64,
    },

    /// Graceful disconnect
    Disconnect {
        reason: String,
    },

    /// Error message
    Error {
        code: u32,
        message: String,
    },
}

impl Message {
    /// Get the message type identifier
    pub fn type_id(&self) -> u8 {
        match self {
            Message::Hello { .. } => 0x01,
            Message::HelloAck { .. } => 0x02,
            Message::MouseMoveRelative { .. } => 0x10,
            Message::MouseMoveAbsolute { .. } => 0x11,
            Message::MouseButton { .. } => 0x12,
            Message::MouseScroll { .. } => 0x13,
            Message::KeyDown { .. } => 0x20,
            Message::KeyUp { .. } => 0x21,
            Message::EnterScreen { .. } => 0x30,
            Message::LeaveScreen { .. } => 0x31,
            Message::ClipboardData { .. } => 0x40,
            Message::ClipboardRequest => 0x41,
            Message::GrabKeyboard => 0x50,
            Message::ReleaseKeyboard => 0x51,
            Message::Heartbeat { .. } => 0xF0,
            Message::HeartbeatAck { .. } => 0xF1,
            Message::Disconnect { .. } => 0xFE,
            Message::Error { .. } => 0xFF,
        }
    }

    /// Check if this is an input event message
    pub fn is_input_event(&self) -> bool {
        matches!(
            self,
            Message::MouseMoveRelative { .. }
                | Message::MouseMoveAbsolute { .. }
                | Message::MouseButton { .. }
                | Message::MouseScroll { .. }
                | Message::KeyDown { .. }
                | Message::KeyUp { .. }
        )
    }
}

/// Error codes for the Error message
pub mod error_codes {
    pub const PROTOCOL_MISMATCH: u32 = 1;
    pub const AUTHENTICATION_FAILED: u32 = 2;
    pub const HOST_NOT_FOUND: u32 = 3;
    pub const CONNECTION_REFUSED: u32 = 4;
    pub const INTERNAL_ERROR: u32 = 100;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modifiers_roundtrip() {
        let mods = Modifiers {
            shift: true,
            ctrl: false,
            alt: true,
            meta: false,
            caps_lock: true,
            num_lock: false,
        };
        let bits = mods.to_bits();
        let restored = Modifiers::from_bits(bits);
        assert_eq!(mods, restored);
    }

    #[test]
    fn test_message_type_ids() {
        let msg = Message::Heartbeat { timestamp: 0 };
        assert_eq!(msg.type_id(), 0xF0);
    }
}
