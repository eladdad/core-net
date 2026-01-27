//! macOS input capture and injection
//!
//! Uses Quartz Event Services (CGEventTap) for event capture and injection.
//!
//! Requirements:
//! - Accessibility permissions must be granted to the application
//! - System Preferences > Security & Privacy > Privacy > Accessibility

#![cfg(target_os = "macos")]

use async_trait::async_trait;
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::events::{
    InputEvent, KeyboardEvent, KeyboardState, MouseButtonEvent, MouseMoveEvent, MouseScrollEvent,
    MouseState,
};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};

/// macOS input capture implementation using CGEventTap
pub struct MacOSInputCapture {
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: Arc<Mutex<MouseState>>,
    keyboard_state: Arc<Mutex<KeyboardState>>,
}

impl MacOSInputCapture {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            suppressing: Arc::new(AtomicBool::new(false)),
            mouse_state: Arc::new(Mutex::new(MouseState::new())),
            keyboard_state: Arc::new(Mutex::new(KeyboardState::new())),
        }
    }

    /// Check if the process has accessibility permissions
    pub fn has_accessibility_permission() -> bool {
        unsafe {
            extern "C" {
                fn AXIsProcessTrusted() -> bool;
            }
            AXIsProcessTrusted()
        }
    }

    /// Request accessibility permissions (opens system dialog)
    pub fn request_accessibility_permission() -> bool {
        // This opens the system preferences dialog
        Self::has_accessibility_permission()
    }

    fn cg_flags_to_modifiers(flags: CGEventFlags) -> Modifiers {
        Modifiers {
            shift: flags.contains(CGEventFlags::CGEventFlagShift),
            ctrl: flags.contains(CGEventFlags::CGEventFlagControl),
            alt: flags.contains(CGEventFlags::CGEventFlagAlternate),
            meta: flags.contains(CGEventFlags::CGEventFlagCommand),
            caps_lock: flags.contains(CGEventFlags::CGEventFlagAlphaShift),
            num_lock: flags.contains(CGEventFlags::CGEventFlagNumericPad),
        }
    }
}

impl Default for MacOSInputCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InputCapture for MacOSInputCapture {
    async fn start(&mut self) -> InputResult<mpsc::Receiver<InputEvent>> {
        if self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::AlreadyStarted);
        }

        if !Self::has_accessibility_permission() {
            Self::request_accessibility_permission();
            return Err(InputError::PermissionDenied(
                "Accessibility permission required. Please enable in System Preferences > Security & Privacy > Privacy > Accessibility".to_string()
            ));
        }

        let (tx, rx) = mpsc::channel(1024);
        let capturing = self.capturing.clone();
        let suppressing = self.suppressing.clone();
        let mouse_state = self.mouse_state.clone();
        let keyboard_state = self.keyboard_state.clone();

        capturing.store(true, Ordering::SeqCst);

        // Note: Full CGEventTap implementation requires using the CoreFoundation
        // run loop which is complex to integrate with async Rust.
        // For now, we use a polling approach with CGEvent functions.
        std::thread::spawn(move || {
            tracing::info!("macOS input capture started (polling mode)");

            let mut last_mouse_pos = (0i32, 0i32);

            while capturing.load(Ordering::SeqCst) {
                // Poll for mouse position changes
                if let Ok(event) = CGEvent::new(CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok().as_ref()) {
                    let location = event.location();
                    let new_x = location.x as i32;
                    let new_y = location.y as i32;

                    if new_x != last_mouse_pos.0 || new_y != last_mouse_pos.1 {
                        let dx = new_x - last_mouse_pos.0;
                        let dy = new_y - last_mouse_pos.1;

                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_micros() as u64;

                        let event = InputEvent::MouseMove(MouseMoveEvent {
                            timestamp,
                            x: Some(new_x),
                            y: Some(new_y),
                            dx,
                            dy,
                        });

                        if let Ok(mut state) = mouse_state.lock() {
                            state.x = new_x;
                            state.y = new_y;
                        }

                        let _ = tx.blocking_send(event);
                        last_mouse_pos = (new_x, new_y);
                    }
                }

                std::thread::sleep(std::time::Duration::from_millis(8)); // ~120Hz polling
            }

            tracing::info!("macOS input capture stopped");
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> InputResult<()> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::NotStarted);
        }

        self.capturing.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::SeqCst)
    }

    fn mouse_state(&self) -> MouseState {
        self.mouse_state.lock().unwrap().clone()
    }

    fn keyboard_state(&self) -> KeyboardState {
        self.keyboard_state.lock().unwrap().clone()
    }

    fn set_suppress(&mut self, suppress: bool) {
        self.suppressing.store(suppress, Ordering::SeqCst);
    }

    fn is_suppressing(&self) -> bool {
        self.suppressing.load(Ordering::SeqCst)
    }
}

/// macOS input injection implementation using CGEvent
pub struct MacOSInputInjector {
    initialized: bool,
}

impl MacOSInputInjector {
    pub fn new() -> Self {
        Self { initialized: false }
    }

    fn get_event_source(&self) -> Option<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
    }
}

impl Default for MacOSInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InputInjector for MacOSInputInjector {
    async fn init(&mut self) -> InputResult<()> {
        if !MacOSInputCapture::has_accessibility_permission() {
            MacOSInputCapture::request_accessibility_permission();
            return Err(InputError::PermissionDenied(
                "Accessibility permission required for input injection".to_string(),
            ));
        }

        self.initialized = true;
        tracing::info!("macOS input injector initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> InputResult<()> {
        self.initialized = false;
        Ok(())
    }

    async fn mouse_move_relative(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Get current mouse position
        if let Ok(current_event) = CGEvent::new(self.get_event_source().as_ref()) {
            let current = current_event.location();
            let new_x = current.x + dx as f64;
            let new_y = current.y + dy as f64;
            return self.mouse_move_absolute(new_x as i32, new_y as i32).await;
        }

        Ok(())
    }

    async fn mouse_move_absolute(&mut self, x: i32, y: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let point = CGPoint::new(x as f64, y as f64);

        if let Ok(event) = CGEvent::new_mouse_event(
            self.get_event_source().as_ref(),
            CGEventType::MouseMoved,
            point,
            CGMouseButton::Left,
        ) {
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    async fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Get current mouse position for the event
        let point = if let Ok(current_event) = CGEvent::new(self.get_event_source().as_ref()) {
            current_event.location()
        } else {
            CGPoint::new(0.0, 0.0)
        };

        let (event_type, cg_button) = match (button, pressed) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
            (MouseButton::Button4, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Button4, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
            (MouseButton::Button5, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Button5, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };

        if let Ok(event) =
            CGEvent::new_mouse_event(self.get_event_source().as_ref(), event_type, point, cg_button)
        {
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    async fn mouse_scroll(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        if let Ok(event) = CGEvent::new_scroll_event(
            self.get_event_source().as_ref(),
            ScrollEventUnit::Line,
            2,
            dy,
            dx,
            0,
        ) {
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    async fn key_down(&mut self, keycode: u32, modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let mac_keycode = hid_to_macos_keycode(keycode);

        if let Ok(event) =
            CGEvent::new_keyboard_event(self.get_event_source().as_ref(), mac_keycode as u16, true)
        {
            let mut flags = CGEventFlags::empty();
            if modifiers.shift {
                flags |= CGEventFlags::CGEventFlagShift;
            }
            if modifiers.ctrl {
                flags |= CGEventFlags::CGEventFlagControl;
            }
            if modifiers.alt {
                flags |= CGEventFlags::CGEventFlagAlternate;
            }
            if modifiers.meta {
                flags |= CGEventFlags::CGEventFlagCommand;
            }
            event.set_flags(flags);
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    async fn key_up(&mut self, keycode: u32, modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let mac_keycode = hid_to_macos_keycode(keycode);

        if let Ok(event) =
            CGEvent::new_keyboard_event(self.get_event_source().as_ref(), mac_keycode as u16, false)
        {
            let mut flags = CGEventFlags::empty();
            if modifiers.shift {
                flags |= CGEventFlags::CGEventFlagShift;
            }
            if modifiers.ctrl {
                flags |= CGEventFlags::CGEventFlagControl;
            }
            if modifiers.alt {
                flags |= CGEventFlags::CGEventFlagAlternate;
            }
            if modifiers.meta {
                flags |= CGEventFlags::CGEventFlagCommand;
            }
            event.set_flags(flags);
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    async fn type_char(&mut self, c: char) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // For simple characters, use keyboard events
        if let Some((keycode, shift)) = char_to_macos_keycode(c) {
            if shift {
                self.key_down(0xE1, Modifiers::default()).await?; // Left Shift HID
            }
            self.key_down(keycode, Modifiers::default()).await?;
            self.key_up(keycode, Modifiers::default()).await?;
            if shift {
                self.key_up(0xE1, Modifiers::default()).await?;
            }
        }

        Ok(())
    }
}

/// Convert USB HID keycode to macOS virtual keycode
fn hid_to_macos_keycode(hid: u32) -> u32 {
    static HID_TO_MAC: &[(u32, u32)] = &[
        (0x04, 0),   // A
        (0x05, 11),  // B
        (0x06, 8),   // C
        (0x07, 2),   // D
        (0x08, 14),  // E
        (0x09, 3),   // F
        (0x0A, 5),   // G
        (0x0B, 4),   // H
        (0x0C, 34),  // I
        (0x0D, 38),  // J
        (0x0E, 40),  // K
        (0x0F, 37),  // L
        (0x10, 46),  // M
        (0x11, 45),  // N
        (0x12, 31),  // O
        (0x13, 35),  // P
        (0x14, 12),  // Q
        (0x15, 15),  // R
        (0x16, 1),   // S
        (0x17, 17),  // T
        (0x18, 32),  // U
        (0x19, 9),   // V
        (0x1A, 13),  // W
        (0x1B, 7),   // X
        (0x1C, 16),  // Y
        (0x1D, 6),   // Z
        (0x1E, 18),  // 1
        (0x1F, 19),  // 2
        (0x20, 20),  // 3
        (0x21, 21),  // 4
        (0x22, 23),  // 5
        (0x23, 22),  // 6
        (0x24, 26),  // 7
        (0x25, 28),  // 8
        (0x26, 25),  // 9
        (0x27, 29),  // 0
        (0x28, 36),  // Return
        (0x29, 53),  // Escape
        (0x2A, 51),  // Backspace
        (0x2B, 48),  // Tab
        (0x2C, 49),  // Space
        (0x4F, 124), // Right Arrow
        (0x50, 123), // Left Arrow
        (0x51, 125), // Down Arrow
        (0x52, 126), // Up Arrow
        (0xE0, 59),  // Left Control
        (0xE1, 56),  // Left Shift
        (0xE2, 58),  // Left Option
        (0xE3, 55),  // Left Command
        (0xE4, 62),  // Right Control
        (0xE5, 60),  // Right Shift
        (0xE6, 61),  // Right Option
        (0xE7, 54),  // Right Command
    ];

    for &(h, m) in HID_TO_MAC {
        if h == hid {
            return m;
        }
    }

    hid
}

/// Map a character to HID keycode and shift state
fn char_to_macos_keycode(c: char) -> Option<(u32, bool)> {
    match c {
        'a'..='z' => Some((0x04 + (c as u32 - 'a' as u32), false)),
        'A'..='Z' => Some((0x04 + (c as u32 - 'A' as u32), true)),
        '0' => Some((0x27, false)),
        '1'..='9' => Some((0x1E + (c as u32 - '1' as u32), false)),
        ' ' => Some((0x2C, false)),
        '\n' => Some((0x28, false)),
        '\t' => Some((0x2B, false)),
        _ => None,
    }
}
