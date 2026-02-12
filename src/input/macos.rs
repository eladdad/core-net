//! macOS input capture and injection
//!
//! Uses Quartz Event Services (CGEventTap) for event capture and injection.
//!
//! Requirements:
//! - Accessibility permissions must be granted to the application
//! - System Preferences > Security & Privacy > Privacy > Accessibility

#![cfg(target_os = "macos")]

use async_trait::async_trait;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::os::raw::{c_int, c_longlong, c_uint, c_ulonglong, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::events::{InputEvent, KeyboardState, MouseMoveEvent, MouseState};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};

/// macOS input capture implementation
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
        #[link(name = "ApplicationServices", kind = "framework")]
        extern "C" {
            fn AXIsProcessTrusted() -> bool;
        }
        unsafe { AXIsProcessTrusted() }
    }

    /// Request accessibility permissions
    pub fn request_accessibility_permission() -> bool {
        Self::has_accessibility_permission()
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
        let mouse_state = self.mouse_state.clone();
        let suppressing = self.suppressing.clone();

        capturing.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            let tap_tx = tx.clone();
            let tap_mouse_state = mouse_state.clone();
            if !spawn_event_tap_loop(tap_tx, tap_mouse_state, capturing.clone(), suppressing.clone()) {
                tracing::warn!("Event tap unavailable, falling back to polling mode");
                polling_capture_loop(capturing, suppressing, mouse_state, tx);
            }
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> InputResult<()> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::NotStarted);
        }

        self.capturing.store(false, Ordering::SeqCst);
        if self.suppressing.swap(false, Ordering::SeqCst) {
            set_cursor_suppression(false);
        }
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
        let was_suppressing = self.suppressing.swap(suppress, Ordering::SeqCst);
        if was_suppressing == suppress {
            tracing::debug!(
                "set_suppress no-op (state unchanged): suppress={}, cursor={}",
                suppress,
                cursor_diag_summary()
            );
            return;
        }

        tracing::info!(
            "set_suppress transition: {} -> {}, cursor={}",
            was_suppressing,
            suppress,
            cursor_diag_summary()
        );
        set_cursor_suppression(suppress);
    }

    fn is_suppressing(&self) -> bool {
        self.suppressing.load(Ordering::SeqCst)
    }
}

impl Drop for MacOSInputCapture {
    fn drop(&mut self) {
        if self.suppressing.swap(false, Ordering::SeqCst) {
            set_cursor_suppression(false);
        }
    }
}

/// macOS input injection implementation using CGEvent
/// 
/// Note: We don't store CGEventSource because it's not Send+Sync.
/// Instead, we create it on-demand for each operation.
pub struct MacOSInputInjector {
    initialized: bool,
}

impl MacOSInputInjector {
    pub fn new() -> Self {
        Self { initialized: false }
    }

    fn create_event_source() -> Option<CGEventSource> {
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

        let (cur_x, cur_y) = get_cursor_position();
        self.mouse_move_absolute(cur_x + dx, cur_y + dy).await
    }

    async fn mouse_move_absolute(&mut self, x: i32, y: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let point = CGPoint::new(x as f64, y as f64);

        if let Some(source) = Self::create_event_source() {
            if let Ok(event) = CGEvent::new_mouse_event(
                source,
                CGEventType::MouseMoved,
                point,
                CGMouseButton::Left,
            ) {
                event.post(CGEventTapLocation::HID);
            }
        }

        Ok(())
    }

    async fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let (x, y) = get_cursor_position();
        let point = CGPoint::new(x as f64, y as f64);

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

        if let Some(source) = Self::create_event_source() {
            if let Ok(event) = CGEvent::new_mouse_event(source, event_type, point, cg_button) {
                event.post(CGEventTapLocation::HID);
            }
        }

        Ok(())
    }

    async fn mouse_scroll(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Use direct FFI for scroll events
        post_scroll_event(dy, dx);
        Ok(())
    }

    async fn key_down(&mut self, keycode: u32, modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let mac_keycode = hid_to_macos_keycode(keycode);

        if let Some(source) = Self::create_event_source() {
            if let Ok(event) = CGEvent::new_keyboard_event(source, mac_keycode as u16, true) {
                set_modifier_flags(&event, &modifiers);
                event.post(CGEventTapLocation::HID);
            }
        }

        Ok(())
    }

    async fn key_up(&mut self, keycode: u32, modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let mac_keycode = hid_to_macos_keycode(keycode);

        if let Some(source) = Self::create_event_source() {
            if let Ok(event) = CGEvent::new_keyboard_event(source, mac_keycode as u16, false) {
                set_modifier_flags(&event, &modifiers);
                event.post(CGEventTapLocation::HID);
            }
        }

        Ok(())
    }

    async fn type_char(&mut self, c: char) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        if let Some((keycode, shift)) = char_to_hid_keycode(c) {
            if shift {
                self.key_down(0xE1, Modifiers::default()).await?;
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

// Helper functions

fn set_cursor_suppression(suppress: bool) {
    unsafe {
        extern "C" {
            fn CGMainDisplayID() -> u32;
            fn CGDisplayHideCursor(display: u32) -> i32;
            fn CGDisplayShowCursor(display: u32) -> i32;
            fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
        }

        let (x, y) = get_cursor_position();
        let main_display_id = CGMainDisplayID();
        let cursor_display = display_for_cursor_point(x, y);

        if suppress {
            let assoc_rc = CGAssociateMouseAndMouseCursorPosition(false);
            let hide_rc = CGDisplayHideCursor(main_display_id);
            tracing::info!(
                assoc_rc = assoc_rc,
                hide_rc = hide_rc,
                main_display = main_display_id,
                cursor_display = ?cursor_display,
                cursor_x = x,
                cursor_y = y,
                "cursor suppress ON"
            );
        } else {
            let assoc_rc = CGAssociateMouseAndMouseCursorPosition(true);
            let show_rc = CGDisplayShowCursor(main_display_id);
            tracing::info!(
                assoc_rc = assoc_rc,
                show_rc = show_rc,
                main_display = main_display_id,
                cursor_display = ?cursor_display,
                cursor_x = x,
                cursor_y = y,
                "cursor suppress OFF"
            );
        }
    }
}

fn cursor_diag_summary() -> String {
    let (x, y) = get_cursor_position();
    let main_display = unsafe {
        extern "C" {
            fn CGMainDisplayID() -> u32;
        }
        CGMainDisplayID()
    };
    let cursor_display = display_for_cursor_point(x, y);
    format!(
        "pos=({}, {}), main_display={}, cursor_display={:?}",
        x, y, main_display, cursor_display
    )
}

fn display_for_cursor_point(x: i32, y: i32) -> Option<u32> {
    unsafe {
        extern "C" {
            fn CGGetDisplaysWithPoint(
                point: CGPoint,
                max_displays: u32,
                displays: *mut u32,
                display_count: *mut u32,
            ) -> i32;
        }

        let point = CGPoint::new(x as f64, y as f64);
        let mut display_id = 0u32;
        let mut display_count = 0u32;
        let rc = CGGetDisplaysWithPoint(point, 1, &mut display_id, &mut display_count);
        if rc == 0 && display_count > 0 {
            Some(display_id)
        } else {
            None
        }
    }
}

fn get_cursor_position() -> (i32, i32) {
    unsafe {
        extern "C" {
            fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            fn CGEventGetLocation(event: *const std::ffi::c_void) -> CGPoint;
            fn CFRelease(cf: *const std::ffi::c_void);
        }

        let event = CGEventCreate(std::ptr::null());
        if !event.is_null() {
            let location = CGEventGetLocation(event);
            CFRelease(event);
            (location.x as i32, location.y as i32)
        } else {
            (0, 0)
        }
    }
}

fn get_main_display_size() -> (i32, i32) {
    unsafe {
        extern "C" {
            fn CGMainDisplayID() -> u32;
            fn CGDisplayBounds(display: u32) -> CGRect;
        }

        #[repr(C)]
        struct CGPoint {
            x: f64,
            y: f64,
        }

        #[repr(C)]
        struct CGSize {
            width: f64,
            height: f64,
        }

        #[repr(C)]
        struct CGRect {
            origin: CGPoint,
            size: CGSize,
        }

        let display = CGMainDisplayID();
        let bounds = CGDisplayBounds(display);
        let w = bounds.size.width as i32;
        let h = bounds.size.height as i32;
        (w.max(1), h.max(1))
    }
}

fn polling_capture_loop(
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: Arc<Mutex<MouseState>>,
    tx: mpsc::Sender<InputEvent>,
) {
    tracing::info!("macOS input capture started (polling mode)");

    let mut last_mouse_pos = (0i32, 0i32);
    let mut suppressing_active = false;
    let (screen_w, screen_h) = get_main_display_size();
    let center_x = (screen_w / 2).max(0);
    let center_y = (screen_h / 2).max(0);
    let edge_margin = 8i32;

    while capturing.load(Ordering::SeqCst) {
        let is_suppressing = suppressing.load(Ordering::SeqCst);

        if is_suppressing && !suppressing_active {
            suppressing_active = true;
            last_mouse_pos = (center_x, center_y);
        } else if !is_suppressing && suppressing_active {
            suppressing_active = false;
            last_mouse_pos = get_cursor_position();
        }

        let (new_x, new_y) = get_cursor_position();

        if new_x != last_mouse_pos.0 || new_y != last_mouse_pos.1 {
            let dx = new_x - last_mouse_pos.0;
            let dy = new_y - last_mouse_pos.1;

            if dx != 0 || dy != 0 {
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
            }

            if suppressing_active {
                let near_edge = new_x <= edge_margin
                    || new_x >= (screen_w - 1 - edge_margin)
                    || new_y <= edge_margin
                    || new_y >= (screen_h - 1 - edge_margin);

                if near_edge {
                    last_mouse_pos = (center_x, center_y);
                } else {
                    last_mouse_pos = (new_x, new_y);
                }
            } else {
                last_mouse_pos = (new_x, new_y);
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(8));
    }

    tracing::info!("macOS input capture stopped");
}

fn spawn_event_tap_loop(
    tx: mpsc::Sender<InputEvent>,
    mouse_state: Arc<Mutex<MouseState>>,
    capturing: Arc<AtomicBool>,
    _suppressing: Arc<AtomicBool>,
) -> bool {
    unsafe {
        type CGEventTapProxy = *mut c_void;
        type CGEventRef = *mut c_void;
        type CFMachPortRef = *mut c_void;
        type CFRunLoopSourceRef = *mut c_void;
        type CFRunLoopRef = *mut c_void;
        type CGEventType = c_uint;
        type CGEventMask = c_ulonglong;

        extern "C" {
            fn CGEventTapCreate(
                tap: c_uint,
                place: c_uint,
                options: c_uint,
                events_of_interest: CGEventMask,
                callback: extern "C" fn(CGEventTapProxy, CGEventType, CGEventRef, *mut c_void) -> CGEventRef,
                user_info: *mut c_void,
            ) -> CFMachPortRef;
            fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
            fn CFMachPortCreateRunLoopSource(
                allocator: *const c_void,
                port: CFMachPortRef,
                order: c_longlong,
            ) -> CFRunLoopSourceRef;
            fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: *const c_void);
            fn CFRunLoopRun();
            fn CFRunLoopStop(rl: CFRunLoopRef);
            fn CFRunLoopGetCurrent() -> CFRunLoopRef;
            fn CFRelease(obj: *const c_void);
            fn CGEventGetIntegerValueField(event: CGEventRef, field: c_int) -> c_longlong;
            static kCFRunLoopCommonModes: *const c_void;
        }

        const KCGHID_EVENT_TAP: c_uint = 0;
        const KCGHEAD_INSERT_EVENT_TAP: c_uint = 0;
        const KCGEVENT_TAP_OPTION_LISTEN_ONLY: c_uint = 1;

        const KCGEVENT_MOUSE_MOVED: CGEventType = 5;
        const KCGEVENT_LEFT_MOUSE_DRAGGED: CGEventType = 6;
        const KCGEVENT_RIGHT_MOUSE_DRAGGED: CGEventType = 7;
        const KCGEVENT_OTHER_MOUSE_DRAGGED: CGEventType = 27;

        const KCGMOUSE_EVENT_DELTA_X: c_int = 4;
        const KCGMOUSE_EVENT_DELTA_Y: c_int = 5;

        #[repr(C)]
        struct TapContext {
            tx: mpsc::Sender<InputEvent>,
            mouse_state: Arc<Mutex<MouseState>>,
            capturing: Arc<AtomicBool>,
        }

        extern "C" fn tap_callback(
            _proxy: CGEventTapProxy,
            event_type: CGEventType,
            event: CGEventRef,
            user_info: *mut c_void,
        ) -> CGEventRef {
            unsafe {
                let context = &*(user_info as *const TapContext);
                if !context.capturing.load(Ordering::SeqCst) {
                    CFRunLoopStop(CFRunLoopGetCurrent());
                    return event;
                }

                if event_type == KCGEVENT_MOUSE_MOVED
                    || event_type == KCGEVENT_LEFT_MOUSE_DRAGGED
                    || event_type == KCGEVENT_RIGHT_MOUSE_DRAGGED
                    || event_type == KCGEVENT_OTHER_MOUSE_DRAGGED
                {
                    let dx = CGEventGetIntegerValueField(event, KCGMOUSE_EVENT_DELTA_X) as i32;
                    let dy = CGEventGetIntegerValueField(event, KCGMOUSE_EVENT_DELTA_Y) as i32;

                    if dx != 0 || dy != 0 {
                        let (x, y) = get_cursor_position();
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_micros() as u64;

                        let event = InputEvent::MouseMove(MouseMoveEvent {
                            timestamp,
                            x: Some(x),
                            y: Some(y),
                            dx,
                            dy,
                        });

                        if let Ok(mut state) = context.mouse_state.lock() {
                            state.x = x;
                            state.y = y;
                        }

                        let _ = context.tx.blocking_send(event);
                    }
                }

                event
            }
        }

        let mask = (1u64 << KCGEVENT_MOUSE_MOVED)
            | (1u64 << KCGEVENT_LEFT_MOUSE_DRAGGED)
            | (1u64 << KCGEVENT_RIGHT_MOUSE_DRAGGED)
            | (1u64 << KCGEVENT_OTHER_MOUSE_DRAGGED);

        let context = Box::new(TapContext {
            tx,
            mouse_state,
            capturing,
        });
        let context_ptr = Box::into_raw(context) as *mut c_void;

        let tap = CGEventTapCreate(
            KCGHID_EVENT_TAP,
            KCGHEAD_INSERT_EVENT_TAP,
            KCGEVENT_TAP_OPTION_LISTEN_ONLY,
            mask,
            tap_callback,
            context_ptr,
        );

        if tap.is_null() {
            let _ = Box::from_raw(context_ptr as *mut TapContext);
            return false;
        }

        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        if source.is_null() {
            CFRelease(tap);
            let _ = Box::from_raw(context_ptr as *mut TapContext);
            return false;
        }

        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);

        tracing::info!("macOS input capture started (CGEventTap)");
        CFRunLoopRun();

        CFRelease(source);
        CFRelease(tap);
        let _ = Box::from_raw(context_ptr as *mut TapContext);

        tracing::info!("macOS input capture stopped (CGEventTap)");
        true
    }
}

fn post_scroll_event(dy: i32, dx: i32) {
    unsafe {
        extern "C" {
            fn CGEventCreateScrollWheelEvent(
                source: *const std::ffi::c_void,
                units: u32,
                wheel_count: u32,
                wheel1: i32,
                ...
            ) -> *mut std::ffi::c_void;
            fn CGEventPost(tap: u32, event: *const std::ffi::c_void);
            fn CFRelease(cf: *const std::ffi::c_void);
        }

        // kCGScrollEventUnitLine = 1, kCGHIDEventTap = 0
        let event = CGEventCreateScrollWheelEvent(std::ptr::null(), 1, 2, dy, dx);
        if !event.is_null() {
            CGEventPost(0, event);
            CFRelease(event);
        }
    }
}

fn set_modifier_flags(event: &CGEvent, modifiers: &Modifiers) {
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
fn char_to_hid_keycode(c: char) -> Option<(u32, bool)> {
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
