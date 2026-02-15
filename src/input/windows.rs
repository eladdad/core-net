//! Windows input capture and injection
//!
//! Uses Raw Input API for capturing actual mouse movement (not just cursor position)
//! and SendInput for event injection.
//!
//! Requirements:
//! - May need to run as Administrator for some operations

#![cfg(target_os = "windows")]

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::events::{
    InputEvent, KeyboardEvent, KeyboardState, MouseButtonEvent, MouseMoveEvent, MouseScrollEvent,
    MouseState,
};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};


/// Windows input capture implementation using Raw Input API
pub struct WindowsInputCapture {
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: Arc<Mutex<MouseState>>,
    keyboard_state: Arc<Mutex<KeyboardState>>,
    cursor_tx: Option<std_mpsc::Sender<CursorCmd>>,
}

impl WindowsInputCapture {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            suppressing: Arc::new(AtomicBool::new(false)),
            mouse_state: Arc::new(Mutex::new(MouseState::new())),
            keyboard_state: Arc::new(Mutex::new(KeyboardState::new())),
            cursor_tx: None,
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
        let keyboard_state = self.keyboard_state.clone();

        capturing.store(true, Ordering::SeqCst);
        self.cursor_tx = Some(spawn_cursor_window_thread());

        // Spawn a thread for the raw input message loop
        std::thread::spawn(move || {
            unsafe {
                use windows::Win32::Foundation::*;
                use windows::Win32::UI::WindowsAndMessaging::*;
                use windows::Win32::UI::Input::*;
                use windows::Win32::System::LibraryLoader::GetModuleHandleW;

                // Windows API constants
                const RIDEV_INPUTSINK: u32 = 0x00000100;

                // Create a message-only window to receive raw input
                let class_name = windows::core::w!("CoreNetRawInput");
                
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(raw_input_wnd_proc),
                    hInstance: GetModuleHandleW(None).unwrap().into(),
                    lpszClassName: class_name,
                    ..Default::default()
                };

                RegisterClassExW(&wc);

                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class_name,
                    windows::core::w!("CoreNet Raw Input"),
                    WINDOW_STYLE::default(),
                    0, 0, 0, 0,
                    HWND_MESSAGE, // Message-only window
                    None,
                    None,
                    None,
                );

                if hwnd.0 == 0 {
                    tracing::error!("Failed to create raw input window");
                    return;
                }

                // Register for raw input
                let devices = [
                    RAWINPUTDEVICE {
                        usUsagePage: 0x01, // HID_USAGE_PAGE_GENERIC
                        usUsage: 0x02,     // HID_USAGE_GENERIC_MOUSE
                        dwFlags: RAWINPUTDEVICE_FLAGS(RIDEV_INPUTSINK),
                        hwndTarget: hwnd,
                    },
                    RAWINPUTDEVICE {
                        usUsagePage: 0x01, // HID_USAGE_PAGE_GENERIC
                        usUsage: 0x06,     // HID_USAGE_GENERIC_KEYBOARD
                        dwFlags: RAWINPUTDEVICE_FLAGS(RIDEV_INPUTSINK),
                        hwndTarget: hwnd,
                    },
                ];

                if !RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_ok() {
                    tracing::error!("Failed to register raw input devices");
                    DestroyWindow(hwnd);
                    return;
                }

                tracing::info!("Windows raw input capture started");

                // Store context in thread-local for the window procedure
                RAW_INPUT_CONTEXT.with(|ctx| {
                    *ctx.borrow_mut() = Some(RawInputContext {
                        tx: tx.clone(),
                        mouse_state: mouse_state.clone(),
                        keyboard_state: keyboard_state.clone(),
                    });
                });

                // Message loop
                let mut msg = MSG::default();
                while capturing.load(Ordering::SeqCst) {
                    // Use PeekMessage with a timeout approach
                    if PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
                        if msg.message == WM_QUIT {
                            break;
                        }
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }

                // Cleanup
                RAW_INPUT_CONTEXT.with(|ctx| {
                    *ctx.borrow_mut() = None;
                });

                DestroyWindow(hwnd);
                tracing::info!("Windows raw input capture stopped");
            }
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> InputResult<()> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::NotStarted);
        }

        self.capturing.store(false, Ordering::SeqCst);
        if let Some(tx) = self.cursor_tx.take() {
            let _ = tx.send(CursorCmd::Show);
            let _ = tx.send(CursorCmd::Quit);
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
            return;
        }
        if let Some(tx) = &self.cursor_tx {
            let cmd = if suppress { CursorCmd::Hide } else { CursorCmd::Show };
            let _ = tx.send(cmd);
        }
    }

    fn is_suppressing(&self) -> bool {
        self.suppressing.load(Ordering::SeqCst)
    }
}

// Thread-local storage for raw input context
struct RawInputContext {
    tx: mpsc::Sender<InputEvent>,
    mouse_state: Arc<Mutex<MouseState>>,
    keyboard_state: Arc<Mutex<KeyboardState>>,
}

thread_local! {
    static RAW_INPUT_CONTEXT: std::cell::RefCell<Option<RawInputContext>> = std::cell::RefCell::new(None);
}

// Window procedure for raw input
unsafe extern "system" fn raw_input_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::UI::Input::*;

    const RID_INPUT: u32 = 0x10000003;
    const RIM_TYPEMOUSE: u32 = 0;
    const RIM_TYPEKEYBOARD: u32 = 1;

    const MOUSE_MOVE_RELATIVE: u16 = 0x00;
    const RI_MOUSE_LEFT_BUTTON_DOWN: u16 = 0x0001;
    const RI_MOUSE_LEFT_BUTTON_UP: u16 = 0x0002;
    const RI_MOUSE_RIGHT_BUTTON_DOWN: u16 = 0x0004;
    const RI_MOUSE_RIGHT_BUTTON_UP: u16 = 0x0008;
    const RI_MOUSE_MIDDLE_BUTTON_DOWN: u16 = 0x0010;
    const RI_MOUSE_MIDDLE_BUTTON_UP: u16 = 0x0020;
    const RI_MOUSE_BUTTON_4_DOWN: u16 = 0x0040;
    const RI_MOUSE_BUTTON_4_UP: u16 = 0x0080;
    const RI_MOUSE_BUTTON_5_DOWN: u16 = 0x0100;
    const RI_MOUSE_BUTTON_5_UP: u16 = 0x0200;
    const RI_MOUSE_WHEEL: u16 = 0x0400;
    const RI_MOUSE_HWHEEL: u16 = 0x0800;

    const RI_KEY_MAKE: u16 = 0;
    const RI_KEY_BREAK: u16 = 1;

    const WM_INPUT: u32 = 0x00FF;

    if msg == WM_INPUT {
        RAW_INPUT_CONTEXT.with(|ctx| {
            if let Some(ref context) = *ctx.borrow() {
                // Get raw input data size
                let mut size: u32 = 0;
                GetRawInputData(
                    HRAWINPUT(lparam.0),
                    RAW_INPUT_DATA_COMMAND_FLAGS(RID_INPUT),
                    None,
                    &mut size,
                    std::mem::size_of::<RAWINPUTHEADER>() as u32,
                );

                if size > 0 {
                    let mut buffer = vec![0u8; size as usize];
                    let bytes_copied = GetRawInputData(
                        HRAWINPUT(lparam.0),
                        RAW_INPUT_DATA_COMMAND_FLAGS(RID_INPUT),
                        Some(buffer.as_mut_ptr() as *mut _),
                        &mut size,
                        std::mem::size_of::<RAWINPUTHEADER>() as u32,
                    );

                    if bytes_copied == size {
                        let raw_input = &*(buffer.as_ptr() as *const RAWINPUT);
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_micros() as u64;

                        match raw_input.header.dwType {
                            RIM_TYPEMOUSE => {
                                let mouse = &raw_input.data.mouse;
                                
                                // Handle relative mouse movement
                                if mouse.usFlags == MOUSE_MOVE_RELATIVE {
                                    let dx = mouse.lLastX;
                                    let dy = mouse.lLastY;
                                    
                                    if dx != 0 || dy != 0 {
                                        // Update internal state
                                        if let Ok(mut state) = context.mouse_state.lock() {
                                            state.x += dx;
                                            state.y += dy;
                                        }
                                        
                                        // Get current cursor position for absolute coords
                                        let mut point = windows::Win32::Foundation::POINT::default();
                                        GetCursorPos(&mut point);

                                        let event = InputEvent::MouseMove(MouseMoveEvent {
                                            timestamp,
                                            x: Some(point.x),
                                            y: Some(point.y),
                                            dx,
                                            dy,
                                        });
                                        let _ = context.tx.blocking_send(event);
                                    }
                                }

                                // Handle mouse buttons
                                let button_flags = mouse.Anonymous.Anonymous.usButtonFlags;
                                
                                if button_flags & RI_MOUSE_LEFT_BUTTON_DOWN != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Left, true);
                                }
                                if button_flags & RI_MOUSE_LEFT_BUTTON_UP != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Left, false);
                                }
                                if button_flags & RI_MOUSE_RIGHT_BUTTON_DOWN != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Right, true);
                                }
                                if button_flags & RI_MOUSE_RIGHT_BUTTON_UP != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Right, false);
                                }
                                if button_flags & RI_MOUSE_MIDDLE_BUTTON_DOWN != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Middle, true);
                                }
                                if button_flags & RI_MOUSE_MIDDLE_BUTTON_UP != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Middle, false);
                                }
                                if button_flags & RI_MOUSE_BUTTON_4_DOWN != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Button4, true);
                                }
                                if button_flags & RI_MOUSE_BUTTON_4_UP != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Button4, false);
                                }
                                if button_flags & RI_MOUSE_BUTTON_5_DOWN != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Button5, true);
                                }
                                if button_flags & RI_MOUSE_BUTTON_5_UP != 0 {
                                    send_mouse_button(&context.tx, timestamp, MouseButton::Button5, false);
                                }

                                // Handle scroll wheel
                                if button_flags & RI_MOUSE_WHEEL != 0 {
                                    let delta = (mouse.Anonymous.Anonymous.usButtonData as i16) as i32 / 120;
                                    let event = InputEvent::MouseScroll(MouseScrollEvent {
                                        timestamp,
                                        dx: 0,
                                        dy: delta,
                                    });
                                    let _ = context.tx.blocking_send(event);
                                }
                                if button_flags & RI_MOUSE_HWHEEL != 0 {
                                    let delta = (mouse.Anonymous.Anonymous.usButtonData as i16) as i32 / 120;
                                    let event = InputEvent::MouseScroll(MouseScrollEvent {
                                        timestamp,
                                        dx: delta,
                                        dy: 0,
                                    });
                                    let _ = context.tx.blocking_send(event);
                                }
                            }
                            RIM_TYPEKEYBOARD => {
                                let keyboard = &raw_input.data.keyboard;
                                let vk = keyboard.VKey;
                                let pressed = keyboard.Flags & RI_KEY_BREAK == 0;
                                let hid_keycode = windows_vk_to_hid(vk as u32);

                                // Update modifier state
                                if let Ok(mut state) = context.keyboard_state.lock() {
                                    match vk {
                                        0xA0 | 0xA1 | 0x10 => state.modifiers.shift = pressed, // VK_LSHIFT, VK_RSHIFT, VK_SHIFT
                                        0xA2 | 0xA3 | 0x11 => state.modifiers.ctrl = pressed,  // VK_LCONTROL, VK_RCONTROL, VK_CONTROL
                                        0xA4 | 0xA5 | 0x12 => state.modifiers.alt = pressed,   // VK_LMENU, VK_RMENU, VK_MENU
                                        0x5B | 0x5C => state.modifiers.meta = pressed,          // VK_LWIN, VK_RWIN
                                        _ => {}
                                    }

                                    let event = InputEvent::Keyboard(KeyboardEvent {
                                        timestamp,
                                        keycode: hid_keycode,
                                        scancode: keyboard.MakeCode as u32,
                                        pressed,
                                        character: None,
                                        modifiers: state.modifiers,
                                    });
                                    let _ = context.tx.blocking_send(event);
                                }
                            }
                            _ => {}
                        }
                    }
                }

            }
        });

        return windows::Win32::Foundation::LRESULT(0);
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

enum CursorCmd {
    Hide,
    Show,
    Quit,
}

fn spawn_cursor_window_thread() -> std_mpsc::Sender<CursorCmd> {
    let (tx, rx) = std_mpsc::channel::<CursorCmd>();

    std::thread::spawn(move || unsafe {
        use windows::Win32::Foundation::*;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        let class_name = windows::core::w!("CoreNetCursorOwner");

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(cursor_owner_wnd_proc),
            hInstance: GetModuleHandleW(None).unwrap().into(),
            lpszClassName: class_name,
            ..Default::default()
        };

        RegisterClassExW(&wc);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0),
            class_name,
            windows::core::w!("CoreNet Cursor Owner"),
            WINDOW_STYLE(WS_POPUP.0),
            0,
            0,
            100,
            100,
            None,
            None,
            None,
            None,
        );

        SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
        ShowWindow(hwnd, SW_HIDE);

        let mut visible = true;

        loop {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            match rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(CursorCmd::Hide) => {
                    if visible {
                        let screen_w = GetSystemMetrics(SM_CXSCREEN);
                        let screen_h = GetSystemMetrics(SM_CYSCREEN);
                        SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            0,
                            0,
                            screen_w,
                            screen_h,
                            SWP_SHOWWINDOW,
                        );
                        ShowWindow(hwnd, SW_SHOW);
                        SetForegroundWindow(hwnd);
                        // SetFocus(hwnd);
                        SetCapture(hwnd);
                        while ShowCursor(false) >= 0 {}
                        tracing::info!("Cursor hidden");
                        visible = false;
                    }
                }
                Ok(CursorCmd::Show) => {
                    if !visible {
                        ReleaseCapture();
                        ShowWindow(hwnd, SW_HIDE);
                        while ShowCursor(true) < 0 {}
                        tracing::info!("Cursor shown");
                        visible = true;
                    }
                }
                Ok(CursorCmd::Quit) => break,
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        DestroyWindow(hwnd);
    });

    tx
}

unsafe extern "system" fn cursor_owner_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::*;

    match msg {
        WM_SETCURSOR => {
            SetCursor(HCURSOR(0));
            windows::Win32::Foundation::LRESULT(1)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn send_mouse_button(tx: &mpsc::Sender<InputEvent>, timestamp: u64, button: MouseButton, pressed: bool) {
    unsafe {
        let mut point = windows::Win32::Foundation::POINT::default();
        windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point);
        
        let event = InputEvent::MouseButton(MouseButtonEvent {
            timestamp,
            button,
            pressed,
            x: point.x,
            y: point.y,
        });
        let _ = tx.blocking_send(event);
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
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE,
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
            (MouseButton::Button4, true) => (MOUSEEVENTF_XDOWN, 1),
            (MouseButton::Button4, false) => (MOUSEEVENTF_XUP, 1),
            (MouseButton::Button5, true) => (MOUSEEVENTF_XDOWN, 2),
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

        if dy != 0 {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: (dy * 120) as u32,
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
