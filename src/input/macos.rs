//! macOS input capture and injection
//!
//! Uses Quartz Event Services (CGEventTap) for event capture and injection.
//!
//! Requirements:
//! - Accessibility permissions must be granted to the application
//! - System Preferences > Security & Privacy > Privacy > Accessibility

#![cfg(target_os = "macos")]

use async_trait::async_trait;
use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopSource};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventType, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::events::{
    EventTimestamp, InputEvent, KeyboardEvent, KeyboardState, MouseButtonEvent, MouseMoveEvent,
    MouseScrollEvent, MouseState,
};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};

/// macOS input capture implementation using CGEventTap
pub struct MacOSInputCapture {
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: Arc<Mutex<MouseState>>,
    keyboard_state: Arc<Mutex<KeyboardState>>,
    event_tap: Option<CGEventTap>,
}

impl MacOSInputCapture {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            suppressing: Arc::new(AtomicBool::new(false)),
            mouse_state: Arc::new(Mutex::new(MouseState::new())),
            keyboard_state: Arc::new(Mutex::new(KeyboardState::new())),
            event_tap: None,
        }
    }

    /// Check if the process has accessibility permissions
    pub fn has_accessibility_permission() -> bool {
        // AXIsProcessTrusted() returns true if the app has accessibility permissions
        unsafe {
            extern "C" {
                fn AXIsProcessTrusted() -> bool;
            }
            AXIsProcessTrusted()
        }
    }

    /// Request accessibility permissions (opens system dialog)
    pub fn request_accessibility_permission() -> bool {
        unsafe {
            extern "C" {
                fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
            }
            
            // Create options dictionary with kAXTrustedCheckOptionPrompt = true
            use core_foundation::dictionary::CFDictionary;
            use core_foundation::boolean::CFBoolean;
            use core_foundation::string::CFString;
            
            let key = CFString::new("AXTrustedCheckOptionPrompt");
            let value = CFBoolean::true_value();
            
            let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
            
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as *const _)
        }
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
            // Request permission
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

        // Spawn the event tap thread
        std::thread::spawn(move || {
            // Define which events to capture
            let event_mask = CGEventType::MouseMoved as u64
                | CGEventType::LeftMouseDown as u64
                | CGEventType::LeftMouseUp as u64
                | CGEventType::RightMouseDown as u64
                | CGEventType::RightMouseUp as u64
                | CGEventType::LeftMouseDragged as u64
                | CGEventType::RightMouseDragged as u64
                | CGEventType::ScrollWheel as u64
                | CGEventType::KeyDown as u64
                | CGEventType::KeyUp as u64
                | CGEventType::FlagsChanged as u64
                | CGEventType::OtherMouseDown as u64
                | CGEventType::OtherMouseUp as u64
                | CGEventType::OtherMouseDragged as u64;

            let tx = Arc::new(Mutex::new(tx));
            let tx_clone = tx.clone();
            let suppressing_clone = suppressing.clone();
            let mouse_state_clone = mouse_state.clone();
            let keyboard_state_clone = keyboard_state.clone();

            // Create callback
            let callback = move |_proxy: *const std::ffi::c_void,
                                 event_type: CGEventType,
                                 event: &CGEvent|
                  -> Option<CGEvent> {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_micros() as u64;

                let location = event.location();
                let flags = event.get_flags();
                let modifiers = MacOSInputCapture::cg_flags_to_modifiers(flags);

                let input_event = match event_type {
                    CGEventType::MouseMoved
                    | CGEventType::LeftMouseDragged
                    | CGEventType::RightMouseDragged
                    | CGEventType::OtherMouseDragged => {
                        let dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32;
                        let dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32;

                        // Update mouse state
                        if let Ok(mut state) = mouse_state_clone.lock() {
                            state.x = location.x as i32;
                            state.y = location.y as i32;
                        }

                        Some(InputEvent::MouseMove(MouseMoveEvent {
                            timestamp,
                            x: Some(location.x as i32),
                            y: Some(location.y as i32),
                            dx,
                            dy,
                        }))
                    }

                    CGEventType::LeftMouseDown => {
                        if let Ok(mut state) = mouse_state_clone.lock() {
                            state.set_button(MouseButton::Left, true);
                        }
                        Some(InputEvent::MouseButton(MouseButtonEvent {
                            timestamp,
                            button: MouseButton::Left,
                            pressed: true,
                            x: location.x as i32,
                            y: location.y as i32,
                        }))
                    }

                    CGEventType::LeftMouseUp => {
                        if let Ok(mut state) = mouse_state_clone.lock() {
                            state.set_button(MouseButton::Left, false);
                        }
                        Some(InputEvent::MouseButton(MouseButtonEvent {
                            timestamp,
                            button: MouseButton::Left,
                            pressed: false,
                            x: location.x as i32,
                            y: location.y as i32,
                        }))
                    }

                    CGEventType::RightMouseDown => {
                        if let Ok(mut state) = mouse_state_clone.lock() {
                            state.set_button(MouseButton::Right, true);
                        }
                        Some(InputEvent::MouseButton(MouseButtonEvent {
                            timestamp,
                            button: MouseButton::Right,
                            pressed: true,
                            x: location.x as i32,
                            y: location.y as i32,
                        }))
                    }

                    CGEventType::RightMouseUp => {
                        if let Ok(mut state) = mouse_state_clone.lock() {
                            state.set_button(MouseButton::Right, false);
                        }
                        Some(InputEvent::MouseButton(MouseButtonEvent {
                            timestamp,
                            button: MouseButton::Right,
                            pressed: false,
                            x: location.x as i32,
                            y: location.y as i32,
                        }))
                    }

                    CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
                        let button_number =
                            event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
                        let button = match button_number {
                            2 => MouseButton::Middle,
                            3 => MouseButton::Button4,
                            4 => MouseButton::Button5,
                            _ => MouseButton::Middle,
                        };
                        let pressed = event_type == CGEventType::OtherMouseDown;

                        if let Ok(mut state) = mouse_state_clone.lock() {
                            state.set_button(button, pressed);
                        }

                        Some(InputEvent::MouseButton(MouseButtonEvent {
                            timestamp,
                            button,
                            pressed,
                            x: location.x as i32,
                            y: location.y as i32,
                        }))
                    }

                    CGEventType::ScrollWheel => {
                        let dy = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1) as i32;
                        let dx = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2) as i32;

                        Some(InputEvent::MouseScroll(MouseScrollEvent {
                            timestamp,
                            dx,
                            dy,
                        }))
                    }

                    CGEventType::KeyDown => {
                        let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u32;
                        let hid_keycode = macos_to_hid_keycode(keycode);

                        if let Ok(mut state) = keyboard_state_clone.lock() {
                            state.key_down(hid_keycode);
                            state.modifiers = modifiers;
                        }

                        Some(InputEvent::Keyboard(KeyboardEvent {
                            timestamp,
                            keycode: hid_keycode,
                            scancode: keycode,
                            pressed: true,
                            character: None,
                            modifiers,
                        }))
                    }

                    CGEventType::KeyUp => {
                        let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u32;
                        let hid_keycode = macos_to_hid_keycode(keycode);

                        if let Ok(mut state) = keyboard_state_clone.lock() {
                            state.key_up(hid_keycode);
                            state.modifiers = modifiers;
                        }

                        Some(InputEvent::Keyboard(KeyboardEvent {
                            timestamp,
                            keycode: hid_keycode,
                            scancode: keycode,
                            pressed: false,
                            character: None,
                            modifiers,
                        }))
                    }

                    CGEventType::FlagsChanged => {
                        if let Ok(mut state) = keyboard_state_clone.lock() {
                            state.modifiers = modifiers;
                        }
                        None // Don't send modifier-only events
                    }

                    _ => None,
                };

                // Send the event
                if let Some(evt) = input_event {
                    if let Ok(sender) = tx_clone.lock() {
                        let _ = sender.blocking_send(evt);
                    }
                }

                // If suppressing, return None to prevent the event from reaching apps
                if suppressing_clone.load(Ordering::SeqCst) {
                    None
                } else {
                    Some(event.clone())
                }
            };

            // Create the event tap
            match CGEventTap::new(
                CGEventTapLocation::HID,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                event_mask,
                callback,
            ) {
                Ok(tap) => {
                    // Create run loop source and add to run loop
                    let source = tap.mach_port_run_loop_source();
                    
                    unsafe {
                        let run_loop = CFRunLoop::get_current();
                        run_loop.add_source(&source, kCFRunLoopCommonModes);
                    }

                    // Enable the tap
                    tap.enable();

                    tracing::info!("macOS event tap created and running");

                    // Run the loop
                    while capturing.load(Ordering::SeqCst) {
                        CFRunLoop::run_in_mode(
                            unsafe { kCFRunLoopDefaultMode },
                            std::time::Duration::from_millis(100),
                            false,
                        );
                    }

                    // Disable and cleanup
                    tap.disable();
                }
                Err(()) => {
                    tracing::error!("Failed to create event tap. Check accessibility permissions.");
                }
            }
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

// Add this for the CFRunLoop mode constant
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: core_foundation::string::CFStringRef;
}

/// macOS input injection implementation using CGEvent
pub struct MacOSInputInjector {
    initialized: bool,
    event_source: Option<CGEventSource>,
}

impl MacOSInputInjector {
    pub fn new() -> Self {
        Self {
            initialized: false,
            event_source: None,
        }
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

        self.event_source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok();
        self.initialized = true;
        
        tracing::info!("macOS input injector initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> InputResult<()> {
        self.event_source = None;
        self.initialized = false;
        Ok(())
    }

    async fn mouse_move_relative(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Get current mouse position
        let display = CGDisplay::main();
        let current = unsafe {
            extern "C" {
                fn CGEventGetLocation(event: *const std::ffi::c_void) -> CGPoint;
            }
            // Create a dummy event to get current position
            if let Ok(event) = CGEvent::new(self.event_source.as_ref()) {
                CGEventGetLocation(event.as_ptr() as *const _)
            } else {
                CGPoint::new(0.0, 0.0)
            }
        };

        let new_x = current.x + dx as f64;
        let new_y = current.y + dy as f64;

        self.mouse_move_absolute(new_x as i32, new_y as i32).await
    }

    async fn mouse_move_absolute(&mut self, x: i32, y: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let point = CGPoint::new(x as f64, y as f64);

        if let Ok(event) = CGEvent::new_mouse_event(
            self.event_source.as_ref(),
            CGEventType::MouseMoved,
            point,
            core_graphics::event::CGMouseButton::Left,
        ) {
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    async fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let display = CGDisplay::main();
        let point = CGPoint::new(0.0, 0.0); // Will use current position

        let (event_type, cg_button) = match (button, pressed) {
            (MouseButton::Left, true) => {
                (CGEventType::LeftMouseDown, core_graphics::event::CGMouseButton::Left)
            }
            (MouseButton::Left, false) => {
                (CGEventType::LeftMouseUp, core_graphics::event::CGMouseButton::Left)
            }
            (MouseButton::Right, true) => {
                (CGEventType::RightMouseDown, core_graphics::event::CGMouseButton::Right)
            }
            (MouseButton::Right, false) => {
                (CGEventType::RightMouseUp, core_graphics::event::CGMouseButton::Right)
            }
            (MouseButton::Middle, true) => {
                (CGEventType::OtherMouseDown, core_graphics::event::CGMouseButton::Center)
            }
            (MouseButton::Middle, false) => {
                (CGEventType::OtherMouseUp, core_graphics::event::CGMouseButton::Center)
            }
            (MouseButton::Button4, true) => {
                (CGEventType::OtherMouseDown, core_graphics::event::CGMouseButton::Center)
            }
            (MouseButton::Button4, false) => {
                (CGEventType::OtherMouseUp, core_graphics::event::CGMouseButton::Center)
            }
            (MouseButton::Button5, true) => {
                (CGEventType::OtherMouseDown, core_graphics::event::CGMouseButton::Center)
            }
            (MouseButton::Button5, false) => {
                (CGEventType::OtherMouseUp, core_graphics::event::CGMouseButton::Center)
            }
        };

        if let Ok(event) =
            CGEvent::new_mouse_event(self.event_source.as_ref(), event_type, point, cg_button)
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
            self.event_source.as_ref(),
            core_graphics::event::ScrollEventUnit::Line,
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
            CGEvent::new_keyboard_event(self.event_source.as_ref(), mac_keycode as u16, true)
        {
            // Set modifier flags
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
            CGEvent::new_keyboard_event(self.event_source.as_ref(), mac_keycode as u16, false)
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

        // Use CGEventKeyboardSetUnicodeString for Unicode characters
        if let Ok(event) = CGEvent::new_keyboard_event(self.event_source.as_ref(), 0, true) {
            let chars = [c as u16];
            unsafe {
                extern "C" {
                    fn CGEventKeyboardSetUnicodeString(
                        event: *mut std::ffi::c_void,
                        length: u64,
                        string: *const u16,
                    );
                }
                CGEventKeyboardSetUnicodeString(event.as_ptr() as *mut _, 1, chars.as_ptr());
            }
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }
}

/// Convert macOS virtual keycode to USB HID keycode
fn macos_to_hid_keycode(mac: u32) -> u32 {
    static MAC_TO_HID: &[(u32, u32)] = &[
        (0, 0x04),   // A
        (11, 0x05),  // B
        (8, 0x06),   // C
        (2, 0x07),   // D
        (14, 0x08),  // E
        (3, 0x09),   // F
        (5, 0x0A),   // G
        (4, 0x0B),   // H
        (34, 0x0C),  // I
        (38, 0x0D),  // J
        (40, 0x0E),  // K
        (37, 0x0F),  // L
        (46, 0x10),  // M
        (45, 0x11),  // N
        (31, 0x12),  // O
        (35, 0x13),  // P
        (12, 0x14),  // Q
        (15, 0x15),  // R
        (1, 0x16),   // S
        (17, 0x17),  // T
        (32, 0x18),  // U
        (9, 0x19),   // V
        (13, 0x1A),  // W
        (7, 0x1B),   // X
        (16, 0x1C),  // Y
        (6, 0x1D),   // Z
        (18, 0x1E),  // 1
        (19, 0x1F),  // 2
        (20, 0x20),  // 3
        (21, 0x21),  // 4
        (23, 0x22),  // 5
        (22, 0x23),  // 6
        (26, 0x24),  // 7
        (28, 0x25),  // 8
        (25, 0x26),  // 9
        (29, 0x27),  // 0
        (36, 0x28),  // Return
        (53, 0x29),  // Escape
        (51, 0x2A),  // Delete (Backspace)
        (48, 0x2B),  // Tab
        (49, 0x2C),  // Space
        (27, 0x2D),  // Minus
        (24, 0x2E),  // Equal
        (33, 0x2F),  // Left Bracket
        (30, 0x30),  // Right Bracket
        (42, 0x31),  // Backslash
        (41, 0x33),  // Semicolon
        (39, 0x34),  // Quote
        (50, 0x35),  // Grave
        (43, 0x36),  // Comma
        (47, 0x37),  // Period
        (44, 0x38),  // Slash
        (57, 0x39),  // Caps Lock
        (122, 0x3A), // F1
        (120, 0x3B), // F2
        (99, 0x3C),  // F3
        (118, 0x3D), // F4
        (96, 0x3E),  // F5
        (97, 0x3F),  // F6
        (98, 0x40),  // F7
        (100, 0x41), // F8
        (101, 0x42), // F9
        (109, 0x43), // F10
        (103, 0x44), // F11
        (111, 0x45), // F12
        (124, 0x4F), // Right Arrow
        (123, 0x50), // Left Arrow
        (125, 0x51), // Down Arrow
        (126, 0x52), // Up Arrow
        (59, 0xE0),  // Left Control
        (56, 0xE1),  // Left Shift
        (58, 0xE2),  // Left Option
        (55, 0xE3),  // Left Command
        (62, 0xE4),  // Right Control
        (60, 0xE5),  // Right Shift
        (61, 0xE6),  // Right Option
        (54, 0xE7),  // Right Command
    ];

    for &(m, h) in MAC_TO_HID {
        if m == mac {
            return h;
        }
    }

    mac
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
        (0x2D, 27),  // Minus
        (0x2E, 24),  // Equal
        (0x2F, 33),  // Left Bracket
        (0x30, 30),  // Right Bracket
        (0x31, 42),  // Backslash
        (0x33, 41),  // Semicolon
        (0x34, 39),  // Quote
        (0x35, 50),  // Grave
        (0x36, 43),  // Comma
        (0x37, 47),  // Period
        (0x38, 44),  // Slash
        (0x39, 57),  // Caps Lock
        (0x3A, 122), // F1
        (0x3B, 120), // F2
        (0x3C, 99),  // F3
        (0x3D, 118), // F4
        (0x3E, 96),  // F5
        (0x3F, 97),  // F6
        (0x40, 98),  // F7
        (0x41, 100), // F8
        (0x42, 101), // F9
        (0x43, 109), // F10
        (0x44, 103), // F11
        (0x45, 111), // F12
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
