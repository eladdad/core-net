//! Linux input capture and injection
//!
//! Uses evdev for event capture and uinput for event injection.
//!
//! Requirements:
//! - User must be in the 'input' group or run as root
//! - /dev/uinput must be accessible for event injection

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::events::{InputEvent, KeyboardState, MouseState};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};

/// Linux input capture implementation using evdev
pub struct LinuxInputCapture {
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: MouseState,
    keyboard_state: KeyboardState,
    /// Paths to input devices being monitored
    device_paths: Vec<String>,
}

impl LinuxInputCapture {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            suppressing: Arc::new(AtomicBool::new(false)),
            mouse_state: MouseState::new(),
            keyboard_state: KeyboardState::new(),
            device_paths: Vec::new(),
        }
    }

    /// Discover input devices on the system
    pub fn discover_devices() -> InputResult<Vec<String>> {
        // In real implementation:
        // - Scan /dev/input/event* devices
        // - Use libevdev to check device capabilities
        // - Return paths to mice and keyboards
        
        let mut devices = Vec::new();
        
        // Check common device paths
        for i in 0..20 {
            let path = format!("/dev/input/event{}", i);
            if std::path::Path::new(&path).exists() {
                devices.push(path);
            }
        }
        
        Ok(devices)
    }

    /// Check if we have permission to access input devices
    pub fn has_permission() -> bool {
        // Check if we can access /dev/input/event0
        // In practice, user needs to be in 'input' group
        std::path::Path::new("/dev/input").exists()
    }

    /// Add a device to monitor
    pub fn add_device(&mut self, path: String) {
        self.device_paths.push(path);
    }
}

impl Default for LinuxInputCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InputCapture for LinuxInputCapture {
    async fn start(&mut self) -> InputResult<mpsc::Receiver<InputEvent>> {
        if self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::AlreadyStarted);
        }

        if !Self::has_permission() {
            return Err(InputError::PermissionDenied(
                "Cannot access /dev/input. Add user to 'input' group: sudo usermod -aG input $USER".to_string()
            ));
        }

        // Auto-discover devices if none specified
        if self.device_paths.is_empty() {
            self.device_paths = Self::discover_devices()?;
        }

        let (tx, rx) = mpsc::channel(1024);
        let capturing = self.capturing.clone();
        let suppressing = self.suppressing.clone();
        let device_paths = self.device_paths.clone();

        capturing.store(true, Ordering::SeqCst);

        // In real implementation:
        // 1. Open each device with libevdev
        // 2. Use epoll/select to wait for events from multiple devices
        // 3. Convert evdev events to InputEvents
        // 4. If suppressing, use EVIOCGRAB to grab exclusive access
        
        tokio::spawn(async move {
            while capturing.load(Ordering::SeqCst) {
                // Poll devices for events
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                
                // The suppressing flag controls whether we grab the device
                let _ = suppressing.load(Ordering::SeqCst);
                let _ = &device_paths;
            }
            drop(tx);
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> InputResult<()> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::NotStarted);
        }

        self.capturing.store(false, Ordering::SeqCst);
        
        // Release grabbed devices
        // Close file descriptors
        
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::SeqCst)
    }

    fn mouse_state(&self) -> MouseState {
        self.mouse_state.clone()
    }

    fn keyboard_state(&self) -> KeyboardState {
        self.keyboard_state.clone()
    }

    fn set_suppress(&mut self, suppress: bool) {
        self.suppressing.store(suppress, Ordering::SeqCst);
        
        // When suppressing, grab the device with EVIOCGRAB
        // When not suppressing, release the grab
    }

    fn is_suppressing(&self) -> bool {
        self.suppressing.load(Ordering::SeqCst)
    }
}

/// Linux input injection implementation using uinput
pub struct LinuxInputInjector {
    initialized: bool,
    /// File descriptor for the uinput device
    uinput_fd: Option<i32>,
}

impl LinuxInputInjector {
    pub fn new() -> Self {
        Self {
            initialized: false,
            uinput_fd: None,
        }
    }

    /// Check if uinput is available
    pub fn is_uinput_available() -> bool {
        std::path::Path::new("/dev/uinput").exists()
    }
}

impl Default for LinuxInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InputInjector for LinuxInputInjector {
    async fn init(&mut self) -> InputResult<()> {
        if !Self::is_uinput_available() {
            return Err(InputError::DeviceNotFound(
                "/dev/uinput not found. Ensure uinput module is loaded: sudo modprobe uinput".to_string()
            ));
        }

        // In real implementation:
        // 1. Open /dev/uinput
        // 2. Set up device with ioctl:
        //    - UI_SET_EVBIT for EV_KEY, EV_REL, EV_ABS
        //    - UI_SET_KEYBIT for all keys
        //    - UI_SET_RELBIT for REL_X, REL_Y, REL_WHEEL
        // 3. Write uinput_setup struct
        // 4. UI_DEV_CREATE
        
        self.initialized = true;
        tracing::info!("Linux input injector initialized via uinput");
        Ok(())
    }

    async fn shutdown(&mut self) -> InputResult<()> {
        // UI_DEV_DESTROY
        // Close file descriptor
        
        self.initialized = false;
        self.uinput_fd = None;
        Ok(())
    }

    async fn mouse_move_relative(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Write input_event structs:
        // { type: EV_REL, code: REL_X, value: dx }
        // { type: EV_REL, code: REL_Y, value: dy }
        // { type: EV_SYN, code: SYN_REPORT, value: 0 }
        
        tracing::debug!("Linux: mouse move relative dx={}, dy={}", dx, dy);
        Ok(())
    }

    async fn mouse_move_absolute(&mut self, x: i32, y: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // For absolute positioning, we need EV_ABS events
        // { type: EV_ABS, code: ABS_X, value: x }
        // { type: EV_ABS, code: ABS_Y, value: y }
        // { type: EV_SYN, code: SYN_REPORT, value: 0 }
        
        tracing::debug!("Linux: mouse move absolute x={}, y={}", x, y);
        Ok(())
    }

    async fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Map MouseButton to BTN_* code
        let _btn_code = match button {
            MouseButton::Left => 0x110,   // BTN_LEFT
            MouseButton::Right => 0x111,  // BTN_RIGHT
            MouseButton::Middle => 0x112, // BTN_MIDDLE
            MouseButton::Button4 => 0x113, // BTN_SIDE
            MouseButton::Button5 => 0x114, // BTN_EXTRA
        };

        // { type: EV_KEY, code: btn_code, value: pressed as i32 }
        // { type: EV_SYN, code: SYN_REPORT, value: 0 }
        
        tracing::debug!("Linux: mouse button {:?} pressed={}", button, pressed);
        Ok(())
    }

    async fn mouse_scroll(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // { type: EV_REL, code: REL_HWHEEL, value: dx }
        // { type: EV_REL, code: REL_WHEEL, value: dy }
        // { type: EV_SYN, code: SYN_REPORT, value: 0 }
        
        tracing::debug!("Linux: mouse scroll dx={}, dy={}", dx, dy);
        Ok(())
    }

    async fn key_down(&mut self, keycode: u32, _modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Convert USB HID keycode to Linux keycode
        let _linux_keycode = hid_to_linux_keycode(keycode);

        // { type: EV_KEY, code: linux_keycode, value: 1 }
        // { type: EV_SYN, code: SYN_REPORT, value: 0 }
        
        tracing::debug!("Linux: key down keycode={:#x}", keycode);
        Ok(())
    }

    async fn key_up(&mut self, keycode: u32, _modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // { type: EV_KEY, code: linux_keycode, value: 0 }
        // { type: EV_SYN, code: SYN_REPORT, value: 0 }
        
        tracing::debug!("Linux: key up keycode={:#x}", keycode);
        Ok(())
    }

    async fn type_char(&mut self, c: char) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Look up keycode for character
        // Handle shift for uppercase letters
        
        tracing::debug!("Linux: type char '{}'", c);
        Ok(())
    }
}

/// Convert USB HID keycode to Linux input keycode
fn hid_to_linux_keycode(hid: u32) -> u32 {
    // This is a simplified mapping - real implementation would have full table
    match hid {
        0x04..=0x1D => hid - 0x04 + 30,  // A-Z -> KEY_A to KEY_Z
        0x1E..=0x27 => hid - 0x1E + 2,   // 1-0 -> KEY_1 to KEY_0
        0x28 => 28,  // Enter -> KEY_ENTER
        0x29 => 1,   // Escape -> KEY_ESC
        0x2A => 14,  // Backspace -> KEY_BACKSPACE
        0x2B => 15,  // Tab -> KEY_TAB
        0x2C => 57,  // Space -> KEY_SPACE
        _ => 0,
    }
}

// Example of reading events from evdev:
/*
fn read_evdev_event(fd: RawFd) -> Option<InputEvent> {
    let mut event = input_event {
        time: timeval { tv_sec: 0, tv_usec: 0 },
        type_: 0,
        code: 0,
        value: 0,
    };
    
    let size = std::mem::size_of::<input_event>();
    let ptr = &mut event as *mut _ as *mut u8;
    
    unsafe {
        let n = libc::read(fd, ptr as *mut libc::c_void, size);
        if n != size as isize {
            return None;
        }
    }
    
    match event.type_ as u32 {
        EV_REL => {
            // Relative movement
            match event.code as u32 {
                REL_X => Some(InputEvent::MouseMove(...)),
                REL_Y => ...,
                REL_WHEEL => Some(InputEvent::MouseScroll(...)),
                _ => None,
            }
        }
        EV_KEY => {
            // Key or button press
            let pressed = event.value != 0;
            if event.code >= BTN_MOUSE && event.code < BTN_JOYSTICK {
                // Mouse button
                Some(InputEvent::MouseButton(...))
            } else {
                // Keyboard key
                Some(InputEvent::Keyboard(...))
            }
        }
        _ => None,
    }
}
*/
