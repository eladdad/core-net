//! Windows input capture and injection
//!
//! Uses low-level hooks (SetWindowsHookEx) for event capture
//! and SendInput for event injection.
//!
//! Requirements:
//! - May need to run as Administrator for global hooks
//! - Some games/applications may block hooks

#![cfg(target_os = "windows")]

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::events::{
    InputEvent, KeyboardEvent, KeyboardState, MouseButtonEvent, MouseMoveEvent, MouseScrollEvent,
    MouseState,
};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};

/// Windows input capture implementation
/// 
/// Note: Full low-level hook implementation requires careful thread management
/// and Win32 message pumping. This is a simplified version that works for
/// input injection while capture uses polling.
pub struct WindowsInputCapture {
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: Arc<Mutex<MouseState>>,
    keyboard_state: Arc<Mutex<KeyboardState>>,
}

impl WindowsInputCapture {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            suppressing: Arc::new(AtomicBool::new(false)),
            mouse_state: Arc::new(Mutex::new(MouseState::new())),
            keyboard_state: Arc::new(Mutex::new(KeyboardState::new())),
        }
    }

    pub fn has_required_privileges() -> bool {
        true
    }
}

impl Default for WindowsInputCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InputCapture for WindowsInputCapture {
    async fn start(&mut self) -> InputResult<mpsc::Receiver<InputEvent>> {
        if self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::AlreadyStarted);
        }

        let (tx, rx) = mpsc::channel(1024);
        let capturing = self.capturing.clone();
        let mouse_state = self.mouse_state.clone();

        capturing.store(true, Ordering::SeqCst);

        // Use polling approach for mouse position
        std::thread::spawn(move || {
            tracing::info!("Windows input capture started (polling mode)");

            let mut last_mouse_pos = (0i32, 0i32);

            while capturing.load(Ordering::SeqCst) {
                // Get current cursor position
                let mut point = windows::Win32::Foundation::POINT { x: 0, y: 0 };
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point);
                }

                let new_x = point.x;
                let new_y = point.y;

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

                std::thread::sleep(std::time::Duration::from_millis(8)); // ~120Hz
            }

            tracing::info!("Windows input capture stopped");
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

/// Windows input injection implementation using SendInput
pub struct WindowsInputInjector {
    initialized: bool,
    screen_width: i32,
    screen_height: i32,
}

impl WindowsInputInjector {
    pub fn new() -> Self {
        Self {
            initialized: false,
            screen_width: 0,
            screen_height: 0,
        }
    }
}

impl Default for WindowsInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InputInjector for WindowsInputInjector {
    async fn init(&mut self) -> InputResult<()> {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
            self.screen_width = GetSystemMetrics(SM_CXSCREEN);
            self.screen_height = GetSystemMetrics(SM_CYSCREEN);
        }

        self.initialized = true;
        tracing::info!(
            "Windows input injector initialized (screen: {}x{})",
            self.screen_width,
            self.screen_height
        );
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

        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }

        Ok(())
    }

    async fn mouse_move_absolute(&mut self, x: i32, y: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        // Convert to normalized coordinates (0-65535)
        let normalized_x = if self.screen_width > 0 {
            (x * 65535) / self.screen_width
        } else {
            x
        };
        let normalized_y = if self.screen_height > 0 {
            (y * 65535) / self.screen_height
        } else {
            y
        };

        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: normalized_x,
                    dy: normalized_y,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }

        Ok(())
    }

    async fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        let (flags, mouse_data) = match (button, pressed) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0u32),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Button4, true) => (MOUSEEVENTF_XDOWN, 1), // XBUTTON1
            (MouseButton::Button4, false) => (MOUSEEVENTF_XUP, 1),
            (MouseButton::Button5, true) => (MOUSEEVENTF_XDOWN, 2), // XBUTTON2
            (MouseButton::Button5, false) => (MOUSEEVENTF_XUP, 2),
        };

        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: mouse_data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }

        Ok(())
    }

    async fn mouse_scroll(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        // Vertical scroll
        if dy != 0 {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: (dy * 120) as u32, // WHEEL_DELTA = 120
                        dwFlags: MOUSEEVENTF_WHEEL,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };

            unsafe {
                SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            }
        }

        // Horizontal scroll
        if dx != 0 {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: (dx * 120) as u32,
                        dwFlags: MOUSEEVENTF_HWHEEL,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };

            unsafe {
                SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            }
        }

        Ok(())
    }

    async fn key_down(&mut self, keycode: u32, _modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        let vk = hid_to_windows_vk(keycode);

        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk as u16),
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }

        Ok(())
    }

    async fn key_up(&mut self, keycode: u32, _modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        let vk = hid_to_windows_vk(keycode);

        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk as u16),
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }

        Ok(())
    }

    async fn type_char(&mut self, c: char) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        // Use Unicode input for character typing
        let inputs = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: c as u16,
                        dwFlags: KEYEVENTF_UNICODE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: c as u16,
                        dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];

        unsafe {
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }

        Ok(())
    }
}

/// Convert USB HID keycode to Windows virtual key code
fn hid_to_windows_vk(hid: u32) -> u32 {
    static HID_TO_VK: &[(u32, u32)] = &[
        (0x04, 0x41), // A
        (0x05, 0x42), // B
        (0x06, 0x43), // C
        (0x07, 0x44), // D
        (0x08, 0x45), // E
        (0x09, 0x46), // F
        (0x0A, 0x47), // G
        (0x0B, 0x48), // H
        (0x0C, 0x49), // I
        (0x0D, 0x4A), // J
        (0x0E, 0x4B), // K
        (0x0F, 0x4C), // L
        (0x10, 0x4D), // M
        (0x11, 0x4E), // N
        (0x12, 0x4F), // O
        (0x13, 0x50), // P
        (0x14, 0x51), // Q
        (0x15, 0x52), // R
        (0x16, 0x53), // S
        (0x17, 0x54), // T
        (0x18, 0x55), // U
        (0x19, 0x56), // V
        (0x1A, 0x57), // W
        (0x1B, 0x58), // X
        (0x1C, 0x59), // Y
        (0x1D, 0x5A), // Z
        (0x1E, 0x31), // 1
        (0x1F, 0x32), // 2
        (0x20, 0x33), // 3
        (0x21, 0x34), // 4
        (0x22, 0x35), // 5
        (0x23, 0x36), // 6
        (0x24, 0x37), // 7
        (0x25, 0x38), // 8
        (0x26, 0x39), // 9
        (0x27, 0x30), // 0
        (0x28, 0x0D), // Enter
        (0x29, 0x1B), // Escape
        (0x2A, 0x08), // Backspace
        (0x2B, 0x09), // Tab
        (0x2C, 0x20), // Space
        (0x4F, 0x27), // Right Arrow
        (0x50, 0x25), // Left Arrow
        (0x51, 0x28), // Down Arrow
        (0x52, 0x26), // Up Arrow
        (0xE0, 0xA2), // Left Ctrl
        (0xE1, 0xA0), // Left Shift
        (0xE2, 0xA4), // Left Alt
        (0xE3, 0x5B), // Left Win
        (0xE4, 0xA3), // Right Ctrl
        (0xE5, 0xA1), // Right Shift
        (0xE6, 0xA5), // Right Alt
        (0xE7, 0x5C), // Right Win
    ];

    for &(h, v) in HID_TO_VK {
        if h == hid {
            return v;
        }
    }

    hid
}
