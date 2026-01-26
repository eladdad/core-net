//! Input event types
//!
//! Platform-independent representation of input events.

use crate::protocol::{Modifiers, MouseButton};

/// Timestamp for events (microseconds since epoch)
pub type EventTimestamp = u64;

/// A mouse movement event
#[derive(Debug, Clone)]
pub struct MouseMoveEvent {
    /// Timestamp of the event
    pub timestamp: EventTimestamp,
    /// Absolute X position (if available)
    pub x: Option<i32>,
    /// Absolute Y position (if available)
    pub y: Option<i32>,
    /// Relative X movement
    pub dx: i32,
    /// Relative Y movement
    pub dy: i32,
}

/// A mouse button event
#[derive(Debug, Clone)]
pub struct MouseButtonEvent {
    pub timestamp: EventTimestamp,
    pub button: MouseButton,
    pub pressed: bool,
    /// Current cursor position
    pub x: i32,
    pub y: i32,
}

/// A mouse scroll event
#[derive(Debug, Clone)]
pub struct MouseScrollEvent {
    pub timestamp: EventTimestamp,
    /// Horizontal scroll delta
    pub dx: i32,
    /// Vertical scroll delta
    pub dy: i32,
}

/// A keyboard event
#[derive(Debug, Clone)]
pub struct KeyboardEvent {
    pub timestamp: EventTimestamp,
    /// Platform-independent keycode (USB HID)
    pub keycode: u32,
    /// Platform-specific scancode
    pub scancode: u32,
    /// Whether the key is pressed (true) or released (false)
    pub pressed: bool,
    /// Unicode character representation (if applicable)
    pub character: Option<char>,
    /// Current modifier state
    pub modifiers: Modifiers,
}

/// Union of all input event types
#[derive(Debug, Clone)]
pub enum InputEvent {
    MouseMove(MouseMoveEvent),
    MouseButton(MouseButtonEvent),
    MouseScroll(MouseScrollEvent),
    Keyboard(KeyboardEvent),
}

impl InputEvent {
    /// Get the timestamp of the event
    pub fn timestamp(&self) -> EventTimestamp {
        match self {
            InputEvent::MouseMove(e) => e.timestamp,
            InputEvent::MouseButton(e) => e.timestamp,
            InputEvent::MouseScroll(e) => e.timestamp,
            InputEvent::Keyboard(e) => e.timestamp,
        }
    }

    /// Check if this is a mouse event
    pub fn is_mouse(&self) -> bool {
        matches!(
            self,
            InputEvent::MouseMove(_) | InputEvent::MouseButton(_) | InputEvent::MouseScroll(_)
        )
    }

    /// Check if this is a keyboard event
    pub fn is_keyboard(&self) -> bool {
        matches!(self, InputEvent::Keyboard(_))
    }
}

/// Current state of a mouse
#[derive(Debug, Clone, Default)]
pub struct MouseState {
    /// Current X position
    pub x: i32,
    /// Current Y position
    pub y: i32,
    /// Button states (index by MouseButton as u8)
    pub buttons: [bool; 5],
}

impl MouseState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_button_pressed(&self, button: MouseButton) -> bool {
        self.buttons[button as usize]
    }

    pub fn set_button(&mut self, button: MouseButton, pressed: bool) {
        self.buttons[button as usize] = pressed;
    }
}

/// Current state of the keyboard
#[derive(Debug, Clone, Default)]
pub struct KeyboardState {
    /// Set of currently pressed keycodes
    pub pressed_keys: std::collections::HashSet<u32>,
    /// Current modifier state
    pub modifiers: Modifiers,
}

impl KeyboardState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn key_down(&mut self, keycode: u32) {
        self.pressed_keys.insert(keycode);
    }

    pub fn key_up(&mut self, keycode: u32) {
        self.pressed_keys.remove(&keycode);
    }

    pub fn is_key_pressed(&self, keycode: u32) -> bool {
        self.pressed_keys.contains(&keycode)
    }
}

/// USB HID Keyboard keycodes (subset of common keys)
pub mod keycodes {
    pub const KEY_A: u32 = 0x04;
    pub const KEY_B: u32 = 0x05;
    pub const KEY_C: u32 = 0x06;
    pub const KEY_D: u32 = 0x07;
    pub const KEY_E: u32 = 0x08;
    pub const KEY_F: u32 = 0x09;
    pub const KEY_G: u32 = 0x0A;
    pub const KEY_H: u32 = 0x0B;
    pub const KEY_I: u32 = 0x0C;
    pub const KEY_J: u32 = 0x0D;
    pub const KEY_K: u32 = 0x0E;
    pub const KEY_L: u32 = 0x0F;
    pub const KEY_M: u32 = 0x10;
    pub const KEY_N: u32 = 0x11;
    pub const KEY_O: u32 = 0x12;
    pub const KEY_P: u32 = 0x13;
    pub const KEY_Q: u32 = 0x14;
    pub const KEY_R: u32 = 0x15;
    pub const KEY_S: u32 = 0x16;
    pub const KEY_T: u32 = 0x17;
    pub const KEY_U: u32 = 0x18;
    pub const KEY_V: u32 = 0x19;
    pub const KEY_W: u32 = 0x1A;
    pub const KEY_X: u32 = 0x1B;
    pub const KEY_Y: u32 = 0x1C;
    pub const KEY_Z: u32 = 0x1D;
    
    pub const KEY_1: u32 = 0x1E;
    pub const KEY_2: u32 = 0x1F;
    pub const KEY_3: u32 = 0x20;
    pub const KEY_4: u32 = 0x21;
    pub const KEY_5: u32 = 0x22;
    pub const KEY_6: u32 = 0x23;
    pub const KEY_7: u32 = 0x24;
    pub const KEY_8: u32 = 0x25;
    pub const KEY_9: u32 = 0x26;
    pub const KEY_0: u32 = 0x27;
    
    pub const KEY_ENTER: u32 = 0x28;
    pub const KEY_ESCAPE: u32 = 0x29;
    pub const KEY_BACKSPACE: u32 = 0x2A;
    pub const KEY_TAB: u32 = 0x2B;
    pub const KEY_SPACE: u32 = 0x2C;
    
    pub const KEY_F1: u32 = 0x3A;
    pub const KEY_F2: u32 = 0x3B;
    pub const KEY_F3: u32 = 0x3C;
    pub const KEY_F4: u32 = 0x3D;
    pub const KEY_F5: u32 = 0x3E;
    pub const KEY_F6: u32 = 0x3F;
    pub const KEY_F7: u32 = 0x40;
    pub const KEY_F8: u32 = 0x41;
    pub const KEY_F9: u32 = 0x42;
    pub const KEY_F10: u32 = 0x43;
    pub const KEY_F11: u32 = 0x44;
    pub const KEY_F12: u32 = 0x45;
    
    pub const KEY_RIGHT_ARROW: u32 = 0x4F;
    pub const KEY_LEFT_ARROW: u32 = 0x50;
    pub const KEY_DOWN_ARROW: u32 = 0x51;
    pub const KEY_UP_ARROW: u32 = 0x52;
    
    pub const KEY_LEFT_CTRL: u32 = 0xE0;
    pub const KEY_LEFT_SHIFT: u32 = 0xE1;
    pub const KEY_LEFT_ALT: u32 = 0xE2;
    pub const KEY_LEFT_META: u32 = 0xE3;
    pub const KEY_RIGHT_CTRL: u32 = 0xE4;
    pub const KEY_RIGHT_SHIFT: u32 = 0xE5;
    pub const KEY_RIGHT_ALT: u32 = 0xE6;
    pub const KEY_RIGHT_META: u32 = 0xE7;
}
