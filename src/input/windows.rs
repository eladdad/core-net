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
use std::ptr;
use tokio::sync::mpsc;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM, BOOL, HWND};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    VIRTUAL_KEY, VK_CAPITAL, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_NUMLOCK, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    MSLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
    WM_MOUSEHWHEEL,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;

use super::events::{
    EventTimestamp, InputEvent, KeyboardEvent, KeyboardState, MouseButtonEvent, MouseMoveEvent,
    MouseScrollEvent, MouseState,
};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};

// Thread-local storage for hook callbacks
thread_local! {
    static HOOK_STATE: std::cell::RefCell<Option<HookState>> = std::cell::RefCell::new(None);
}

struct HookState {
    sender: mpsc::Sender<InputEvent>,
    suppressing: Arc<AtomicBool>,
    mouse_x: i32,
    mouse_y: i32,
    modifiers: Modifiers,
}

/// Windows input capture implementation using low-level hooks
pub struct WindowsInputCapture {
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: Arc<Mutex<MouseState>>,
    keyboard_state: Arc<Mutex<KeyboardState>>,
    mouse_hook: Option<HHOOK>,
    keyboard_hook: Option<HHOOK>,
}

impl WindowsInputCapture {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            suppressing: Arc::new(AtomicBool::new(false)),
            mouse_state: Arc::new(Mutex::new(MouseState::new())),
            keyboard_state: Arc::new(Mutex::new(KeyboardState::new())),
            mouse_hook: None,
            keyboard_hook: None,
        }
    }

    /// Check if we're running with sufficient privileges
    pub fn has_required_privileges() -> bool {
        // Low-level hooks work without admin rights in most cases
        true
    }

    fn get_modifiers(&self) -> Modifiers {
        if let Ok(state) = self.keyboard_state.lock() {
            state.modifiers
        } else {
            Modifiers::default()
        }
    }
}

impl Default for WindowsInputCapture {
    fn default() -> Self {
        Self::new()
    }
}

// Low-level mouse hook callback
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let timestamp = info.time as u64 * 1000; // Convert to microseconds

        HOOK_STATE.with(|state| {
            if let Some(ref mut hook_state) = *state.borrow_mut() {
                let old_x = hook_state.mouse_x;
                let old_y = hook_state.mouse_y;
                hook_state.mouse_x = info.pt.x;
                hook_state.mouse_y = info.pt.y;

                let event = match wparam.0 as u32 {
                    WM_MOUSEMOVE => Some(InputEvent::MouseMove(MouseMoveEvent {
                        timestamp,
                        x: Some(info.pt.x),
                        y: Some(info.pt.y),
                        dx: info.pt.x - old_x,
                        dy: info.pt.y - old_y,
                    })),
                    WM_LBUTTONDOWN => Some(InputEvent::MouseButton(MouseButtonEvent {
                        timestamp,
                        button: MouseButton::Left,
                        pressed: true,
                        x: info.pt.x,
                        y: info.pt.y,
                    })),
                    WM_LBUTTONUP => Some(InputEvent::MouseButton(MouseButtonEvent {
                        timestamp,
                        button: MouseButton::Left,
                        pressed: false,
                        x: info.pt.x,
                        y: info.pt.y,
                    })),
                    WM_RBUTTONDOWN => Some(InputEvent::MouseButton(MouseButtonEvent {
                        timestamp,
                        button: MouseButton::Right,
                        pressed: true,
                        x: info.pt.x,
                        y: info.pt.y,
                    })),
                    WM_RBUTTONUP => Some(InputEvent::MouseButton(MouseButtonEvent {
                        timestamp,
                        button: MouseButton::Right,
                        pressed: false,
                        x: info.pt.x,
                        y: info.pt.y,
                    })),
                    WM_MBUTTONDOWN => Some(InputEvent::MouseButton(MouseButtonEvent {
                        timestamp,
                        button: MouseButton::Middle,
                        pressed: true,
                        x: info.pt.x,
                        y: info.pt.y,
                    })),
                    WM_MBUTTONUP => Some(InputEvent::MouseButton(MouseButtonEvent {
                        timestamp,
                        button: MouseButton::Middle,
                        pressed: false,
                        x: info.pt.x,
                        y: info.pt.y,
                    })),
                    WM_MOUSEWHEEL => {
                        let delta = ((info.mouseData >> 16) as i16) as i32 / 120;
                        Some(InputEvent::MouseScroll(MouseScrollEvent {
                            timestamp,
                            dx: 0,
                            dy: delta,
                        }))
                    }
                    WM_MOUSEHWHEEL => {
                        let delta = ((info.mouseData >> 16) as i16) as i32 / 120;
                        Some(InputEvent::MouseScroll(MouseScrollEvent {
                            timestamp,
                            dx: delta,
                            dy: 0,
                        }))
                    }
                    WM_XBUTTONDOWN => {
                        let button = if (info.mouseData >> 16) & XBUTTON1 as u32 != 0 {
                            MouseButton::Button4
                        } else {
                            MouseButton::Button5
                        };
                        Some(InputEvent::MouseButton(MouseButtonEvent {
                            timestamp,
                            button,
                            pressed: true,
                            x: info.pt.x,
                            y: info.pt.y,
                        }))
                    }
                    WM_XBUTTONUP => {
                        let button = if (info.mouseData >> 16) & XBUTTON1 as u32 != 0 {
                            MouseButton::Button4
                        } else {
                            MouseButton::Button5
                        };
                        Some(InputEvent::MouseButton(MouseButtonEvent {
                            timestamp,
                            button,
                            pressed: false,
                            x: info.pt.x,
                            y: info.pt.y,
                        }))
                    }
                    _ => None,
                };

                if let Some(evt) = event {
                    let _ = hook_state.sender.blocking_send(evt);
                }

                if hook_state.suppressing.load(Ordering::SeqCst) {
                    return LRESULT(1); // Block the input
                }
            }
        });
    }

    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

// Low-level keyboard hook callback
unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let timestamp = info.time as u64 * 1000;

        HOOK_STATE.with(|state| {
            if let Some(ref mut hook_state) = *state.borrow_mut() {
                let vk = VIRTUAL_KEY(info.vkCode as u16);
                let pressed = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;

                // Update modifier state
                match vk {
                    VK_SHIFT | VK_LSHIFT | VK_RSHIFT => hook_state.modifiers.shift = pressed,
                    VK_CONTROL | VK_LCONTROL | VK_RCONTROL => hook_state.modifiers.ctrl = pressed,
                    VK_MENU | VK_LMENU | VK_RMENU => hook_state.modifiers.alt = pressed,
                    VK_LWIN | VK_RWIN => hook_state.modifiers.meta = pressed,
                    VK_CAPITAL if pressed => {
                        hook_state.modifiers.caps_lock = !hook_state.modifiers.caps_lock
                    }
                    VK_NUMLOCK if pressed => {
                        hook_state.modifiers.num_lock = !hook_state.modifiers.num_lock
                    }
                    _ => {}
                }

                let hid_keycode = windows_vk_to_hid(info.vkCode);

                let event = InputEvent::Keyboard(KeyboardEvent {
                    timestamp,
                    keycode: hid_keycode,
                    scancode: info.scanCode,
                    pressed,
                    character: None,
                    modifiers: hook_state.modifiers,
                });

                let _ = hook_state.sender.blocking_send(event);

                if hook_state.suppressing.load(Ordering::SeqCst) {
                    return LRESULT(1);
                }
            }
        });
    }

    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

#[async_trait]
impl InputCapture for WindowsInputCapture {
    async fn start(&mut self) -> InputResult<mpsc::Receiver<InputEvent>> {
        if self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::AlreadyStarted);
        }

        let (tx, rx) = mpsc::channel(1024);
        let capturing = self.capturing.clone();
        let suppressing = self.suppressing.clone();

        capturing.store(true, Ordering::SeqCst);

        // Spawn the hook thread
        std::thread::spawn(move || {
            // Initialize hook state
            HOOK_STATE.with(|state| {
                *state.borrow_mut() = Some(HookState {
                    sender: tx,
                    suppressing,
                    mouse_x: 0,
                    mouse_y: 0,
                    modifiers: Modifiers::default(),
                });
            });

            unsafe {
                let hinstance = GetModuleHandleW(None).ok();

                // Set up mouse hook
                let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), hinstance, 0);

                // Set up keyboard hook
                let keyboard_hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), hinstance, 0);

                if mouse_hook.is_err() || keyboard_hook.is_err() {
                    tracing::error!("Failed to set up input hooks");
                    return;
                }

                let mouse_hook = mouse_hook.unwrap();
                let keyboard_hook = keyboard_hook.unwrap();

                tracing::info!("Windows input hooks installed");

                // Message loop
                let mut msg = MSG::default();
                while capturing.load(Ordering::SeqCst) {
                    if GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
                        // Process messages
                    } else {
                        break;
                    }
                }

                // Clean up hooks
                let _ = UnhookWindowsHookEx(mouse_hook);
                let _ = UnhookWindowsHookEx(keyboard_hook);

                tracing::info!("Windows input hooks removed");
            }

            // Clean up hook state
            HOOK_STATE.with(|state| {
                *state.borrow_mut() = None;
            });
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> InputResult<()> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::NotStarted);
        }

        self.capturing.store(false, Ordering::SeqCst);

        // Post a quit message to exit the message loop
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                GetCurrentThreadId(),
                windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                WPARAM(0),
                LPARAM(0),
            );
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

        let mut input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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

        // Convert to normalized coordinates (0-65535)
        let normalized_x = (x * 65535) / self.screen_width;
        let normalized_y = (y * 65535) / self.screen_height;

        let mut input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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

        let (flags, mouse_data) = match (button, pressed) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Button4, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
            (MouseButton::Button4, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
            (MouseButton::Button5, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
            (MouseButton::Button5, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
        };

        let mut input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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

        // Vertical scroll
        if dy != 0 {
            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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
            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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

        let vk = hid_to_windows_vk(keycode);

        let mut input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk as u16),
                    wScan: 0,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0),
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

        let vk = hid_to_windows_vk(keycode);

        let mut input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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

        // Use Unicode input
        let mut inputs = vec![
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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
                Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
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

/// Convert Windows virtual key code to USB HID keycode
fn windows_vk_to_hid(vk: u32) -> u32 {
    static VK_TO_HID: &[(u32, u32)] = &[
        (0x41, 0x04), // A
        (0x42, 0x05), // B
        (0x43, 0x06), // C
        (0x44, 0x07), // D
        (0x45, 0x08), // E
        (0x46, 0x09), // F
        (0x47, 0x0A), // G
        (0x48, 0x0B), // H
        (0x49, 0x0C), // I
        (0x4A, 0x0D), // J
        (0x4B, 0x0E), // K
        (0x4C, 0x0F), // L
        (0x4D, 0x10), // M
        (0x4E, 0x11), // N
        (0x4F, 0x12), // O
        (0x50, 0x13), // P
        (0x51, 0x14), // Q
        (0x52, 0x15), // R
        (0x53, 0x16), // S
        (0x54, 0x17), // T
        (0x55, 0x18), // U
        (0x56, 0x19), // V
        (0x57, 0x1A), // W
        (0x58, 0x1B), // X
        (0x59, 0x1C), // Y
        (0x5A, 0x1D), // Z
        (0x31, 0x1E), // 1
        (0x32, 0x1F), // 2
        (0x33, 0x20), // 3
        (0x34, 0x21), // 4
        (0x35, 0x22), // 5
        (0x36, 0x23), // 6
        (0x37, 0x24), // 7
        (0x38, 0x25), // 8
        (0x39, 0x26), // 9
        (0x30, 0x27), // 0
        (0x0D, 0x28), // Enter
        (0x1B, 0x29), // Escape
        (0x08, 0x2A), // Backspace
        (0x09, 0x2B), // Tab
        (0x20, 0x2C), // Space
        (0xBD, 0x2D), // Minus
        (0xBB, 0x2E), // Equal
        (0xDB, 0x2F), // Left Bracket
        (0xDD, 0x30), // Right Bracket
        (0xDC, 0x31), // Backslash
        (0xBA, 0x33), // Semicolon
        (0xDE, 0x34), // Quote
        (0xC0, 0x35), // Grave
        (0xBC, 0x36), // Comma
        (0xBE, 0x37), // Period
        (0xBF, 0x38), // Slash
        (0x14, 0x39), // Caps Lock
        (0x70, 0x3A), // F1
        (0x71, 0x3B), // F2
        (0x72, 0x3C), // F3
        (0x73, 0x3D), // F4
        (0x74, 0x3E), // F5
        (0x75, 0x3F), // F6
        (0x76, 0x40), // F7
        (0x77, 0x41), // F8
        (0x78, 0x42), // F9
        (0x79, 0x43), // F10
        (0x7A, 0x44), // F11
        (0x7B, 0x45), // F12
        (0x27, 0x4F), // Right Arrow
        (0x25, 0x50), // Left Arrow
        (0x28, 0x51), // Down Arrow
        (0x26, 0x52), // Up Arrow
        (0xA2, 0xE0), // Left Ctrl
        (0xA0, 0xE1), // Left Shift
        (0xA4, 0xE2), // Left Alt
        (0x5B, 0xE3), // Left Win
        (0xA3, 0xE4), // Right Ctrl
        (0xA1, 0xE5), // Right Shift
        (0xA5, 0xE6), // Right Alt
        (0x5C, 0xE7), // Right Win
    ];

    for &(v, h) in VK_TO_HID {
        if v == vk {
            return h;
        }
    }

    vk
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
        (0x2D, 0xBD), // Minus
        (0x2E, 0xBB), // Equal
        (0x2F, 0xDB), // Left Bracket
        (0x30, 0xDD), // Right Bracket
        (0x31, 0xDC), // Backslash
        (0x33, 0xBA), // Semicolon
        (0x34, 0xDE), // Quote
        (0x35, 0xC0), // Grave
        (0x36, 0xBC), // Comma
        (0x37, 0xBE), // Period
        (0x38, 0xBF), // Slash
        (0x39, 0x14), // Caps Lock
        (0x3A, 0x70), // F1
        (0x3B, 0x71), // F2
        (0x3C, 0x72), // F3
        (0x3D, 0x73), // F4
        (0x3E, 0x74), // F5
        (0x3F, 0x75), // F6
        (0x40, 0x76), // F7
        (0x41, 0x77), // F8
        (0x42, 0x78), // F9
        (0x43, 0x79), // F10
        (0x44, 0x7A), // F11
        (0x45, 0x7B), // F12
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
