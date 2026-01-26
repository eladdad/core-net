//! Linux input capture and injection
//!
//! Uses evdev for event capture and uinput for event injection.
//!
//! Requirements:
//! - User must be in the 'input' group or run as root
//! - /dev/uinput must be accessible for event injection

use async_trait::async_trait;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use super::events::{
    EventTimestamp, InputEvent, KeyboardEvent, KeyboardState, MouseButtonEvent, MouseMoveEvent,
    MouseScrollEvent, MouseState,
};
use super::traits::{InputCapture, InputError, InputInjector, InputResult};
use crate::protocol::{Modifiers, MouseButton};

// Linux input event constants
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const SYN_REPORT: u16 = 0x00;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;
const REL_HWHEEL: u16 = 0x06;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

// Mouse button codes
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const BTN_MOUSE: u16 = 0x110;

// Key codes for modifiers
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTALT: u16 = 56;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_RIGHTALT: u16 = 100;
const KEY_RIGHTMETA: u16 = 126;
const KEY_CAPSLOCK: u16 = 58;
const KEY_NUMLOCK: u16 = 69;

// uinput ioctl constants
const UINPUT_MAX_NAME_SIZE: usize = 80;

/// Raw input_event structure (matches Linux kernel structure)
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct InputEventRaw {
    tv_sec: i64,
    tv_usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

impl InputEventRaw {
    fn new(type_: u16, code: u16, value: i32) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        Self {
            tv_sec: now.as_secs() as i64,
            tv_usec: now.subsec_micros() as i64,
            type_,
            code,
            value,
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(std::mem::size_of::<Self>());
        bytes.extend_from_slice(&self.tv_sec.to_ne_bytes());
        bytes.extend_from_slice(&self.tv_usec.to_ne_bytes());
        bytes.extend_from_slice(&self.type_.to_ne_bytes());
        bytes.extend_from_slice(&self.code.to_ne_bytes());
        bytes.extend_from_slice(&self.value.to_ne_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < std::mem::size_of::<Self>() {
            return None;
        }
        Some(Self {
            tv_sec: i64::from_ne_bytes(bytes[0..8].try_into().ok()?),
            tv_usec: i64::from_ne_bytes(bytes[8..16].try_into().ok()?),
            type_: u16::from_ne_bytes(bytes[16..18].try_into().ok()?),
            code: u16::from_ne_bytes(bytes[18..20].try_into().ok()?),
            value: i32::from_ne_bytes(bytes[20..24].try_into().ok()?),
        })
    }

    fn timestamp_us(&self) -> u64 {
        (self.tv_sec as u64) * 1_000_000 + (self.tv_usec as u64)
    }
}

/// uinput_setup structure
#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; UINPUT_MAX_NAME_SIZE],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

/// Linux input capture implementation using evdev
pub struct LinuxInputCapture {
    capturing: Arc<AtomicBool>,
    suppressing: Arc<AtomicBool>,
    mouse_state: MouseState,
    keyboard_state: KeyboardState,
    device_paths: Vec<String>,
    grabbed_devices: Vec<RawFd>,
}

impl LinuxInputCapture {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            suppressing: Arc::new(AtomicBool::new(false)),
            mouse_state: MouseState::new(),
            keyboard_state: KeyboardState::new(),
            device_paths: Vec::new(),
            grabbed_devices: Vec::new(),
        }
    }

    /// Discover input devices on the system
    pub fn discover_devices() -> InputResult<Vec<String>> {
        let mut devices = Vec::new();
        let input_dir = Path::new("/dev/input");

        if !input_dir.exists() {
            return Err(InputError::DeviceNotFound(
                "/dev/input directory not found".to_string(),
            ));
        }

        // Look for event devices
        for entry in std::fs::read_dir(input_dir).map_err(|e| InputError::Io(e))? {
            let entry = entry.map_err(|e| InputError::Io(e))?;
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if name.starts_with("event") {
                // Check if it's a mouse or keyboard by reading capabilities
                if let Ok(device_name) = Self::get_device_name(&path) {
                    let lower_name = device_name.to_lowercase();
                    // Include mice, keyboards, touchpads
                    if lower_name.contains("mouse")
                        || lower_name.contains("keyboard")
                        || lower_name.contains("touchpad")
                        || lower_name.contains("trackpad")
                        || lower_name.contains("pointer")
                    {
                        devices.push(path.to_string_lossy().to_string());
                        tracing::debug!("Found input device: {} ({})", path.display(), device_name);
                    }
                }
            }
        }

        // If no specific devices found, try to detect by capabilities
        if devices.is_empty() {
            for i in 0..20 {
                let path = format!("/dev/input/event{}", i);
                if Path::new(&path).exists() {
                    if Self::device_has_mouse_or_keyboard_caps(&path) {
                        devices.push(path);
                    }
                }
            }
        }

        if devices.is_empty() {
            return Err(InputError::DeviceNotFound(
                "No input devices found. Make sure you're in the 'input' group.".to_string(),
            ));
        }

        Ok(devices)
    }

    fn get_device_name(path: &Path) -> std::io::Result<String> {
        // Read device name from sysfs
        let event_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let sysfs_path = format!("/sys/class/input/{}/device/name", event_name);

        if Path::new(&sysfs_path).exists() {
            std::fs::read_to_string(&sysfs_path).map(|s| s.trim().to_string())
        } else {
            Ok(String::new())
        }
    }

    fn device_has_mouse_or_keyboard_caps(path: &str) -> bool {
        // Try to open and check if device produces relevant events
        if let Ok(file) = File::open(path) {
            // Use EVIOCGBIT to check capabilities
            // For simplicity, we'll just check if we can open it
            drop(file);
            true
        } else {
            false
        }
    }

    /// Check if we have permission to access input devices
    pub fn has_permission() -> bool {
        // Check if we can access /dev/input/event0
        if let Ok(entries) = std::fs::read_dir("/dev/input") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.to_string_lossy().contains("event") {
                    if File::open(&path).is_ok() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Add a device to monitor
    pub fn add_device(&mut self, path: String) {
        if !self.device_paths.contains(&path) {
            self.device_paths.push(path);
        }
    }

    fn update_modifiers(&mut self, code: u16, pressed: bool) {
        match code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => self.keyboard_state.modifiers.shift = pressed,
            KEY_LEFTCTRL | KEY_RIGHTCTRL => self.keyboard_state.modifiers.ctrl = pressed,
            KEY_LEFTALT | KEY_RIGHTALT => self.keyboard_state.modifiers.alt = pressed,
            KEY_LEFTMETA | KEY_RIGHTMETA => self.keyboard_state.modifiers.meta = pressed,
            KEY_CAPSLOCK if pressed => {
                self.keyboard_state.modifiers.caps_lock = !self.keyboard_state.modifiers.caps_lock
            }
            KEY_NUMLOCK if pressed => {
                self.keyboard_state.modifiers.num_lock = !self.keyboard_state.modifiers.num_lock
            }
            _ => {}
        }
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
                "Cannot access /dev/input devices. Add user to 'input' group: sudo usermod -aG input $USER".to_string()
            ));
        }

        // Auto-discover devices if none specified
        if self.device_paths.is_empty() {
            self.device_paths = Self::discover_devices()?;
        }

        tracing::info!("Starting input capture on {} devices", self.device_paths.len());

        let (tx, rx) = mpsc::channel(1024);
        let capturing = self.capturing.clone();
        let suppressing = self.suppressing.clone();
        let device_paths = self.device_paths.clone();

        capturing.store(true, Ordering::SeqCst);

        // Spawn the capture thread
        std::thread::spawn(move || {
            let mut files: Vec<(String, File, RawFd)> = Vec::new();
            let mut mouse_x: i32 = 0;
            let mut mouse_y: i32 = 0;
            let mut modifiers = Modifiers::default();

            // Open all devices
            for path in &device_paths {
                match OpenOptions::new().read(true).open(path) {
                    Ok(file) => {
                        let fd = file.as_raw_fd();
                        
                        // Set non-blocking
                        unsafe {
                            let flags = libc::fcntl(fd, libc::F_GETFL);
                            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                        }
                        
                        files.push((path.clone(), file, fd));
                        tracing::debug!("Opened device: {}", path);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to open {}: {}", path, e);
                    }
                }
            }

            if files.is_empty() {
                tracing::error!("No input devices could be opened");
                return;
            }

            let event_size = std::mem::size_of::<InputEventRaw>();
            let mut buf = vec![0u8; event_size];

            // Accumulated relative movements for batching
            let mut rel_x: i32 = 0;
            let mut rel_y: i32 = 0;

            while capturing.load(Ordering::SeqCst) {
                let mut had_event = false;

                for (path, file, fd) in &mut files {
                    // Try to read events
                    loop {
                        let file_ref: &mut File = file;
                        match file_ref.read_exact(&mut buf) {
                            Ok(_) => {
                                if let Some(raw_event) = InputEventRaw::from_bytes(&buf) {
                                    had_event = true;
                                    let timestamp = raw_event.timestamp_us();

                                    match raw_event.type_ {
                                        EV_REL => {
                                            match raw_event.code {
                                                REL_X => {
                                                    rel_x += raw_event.value;
                                                    mouse_x += raw_event.value;
                                                }
                                                REL_Y => {
                                                    rel_y += raw_event.value;
                                                    mouse_y += raw_event.value;
                                                }
                                                REL_WHEEL => {
                                                    let event = InputEvent::MouseScroll(
                                                        MouseScrollEvent {
                                                            timestamp,
                                                            dx: 0,
                                                            dy: raw_event.value,
                                                        },
                                                    );
                                                    let _ = tx.blocking_send(event);
                                                }
                                                REL_HWHEEL => {
                                                    let event = InputEvent::MouseScroll(
                                                        MouseScrollEvent {
                                                            timestamp,
                                                            dx: raw_event.value,
                                                            dy: 0,
                                                        },
                                                    );
                                                    let _ = tx.blocking_send(event);
                                                }
                                                _ => {}
                                            }
                                        }
                                        EV_KEY => {
                                            let pressed = raw_event.value != 0;
                                            let code = raw_event.code;

                                            // Check if it's a mouse button
                                            if code >= BTN_MOUSE && code < BTN_MOUSE + 8 {
                                                let button = match code {
                                                    BTN_LEFT => MouseButton::Left,
                                                    BTN_RIGHT => MouseButton::Right,
                                                    BTN_MIDDLE => MouseButton::Middle,
                                                    BTN_SIDE => MouseButton::Button4,
                                                    BTN_EXTRA => MouseButton::Button5,
                                                    _ => continue,
                                                };

                                                let event = InputEvent::MouseButton(
                                                    MouseButtonEvent {
                                                        timestamp,
                                                        button,
                                                        pressed,
                                                        x: mouse_x,
                                                        y: mouse_y,
                                                    },
                                                );
                                                let _ = tx.blocking_send(event);
                                            } else {
                                                // Keyboard event
                                                // Update modifier state
                                                match code {
                                                    KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                                                        modifiers.shift = pressed
                                                    }
                                                    KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                                                        modifiers.ctrl = pressed
                                                    }
                                                    KEY_LEFTALT | KEY_RIGHTALT => {
                                                        modifiers.alt = pressed
                                                    }
                                                    KEY_LEFTMETA | KEY_RIGHTMETA => {
                                                        modifiers.meta = pressed
                                                    }
                                                    _ => {}
                                                }

                                                let keycode = linux_to_hid_keycode(code as u32);
                                                let event =
                                                    InputEvent::Keyboard(KeyboardEvent {
                                                        timestamp,
                                                        keycode,
                                                        scancode: code as u32,
                                                        pressed,
                                                        character: None,
                                                        modifiers,
                                                    });
                                                let _ = tx.blocking_send(event);
                                            }
                                        }
                                        EV_SYN if raw_event.code == SYN_REPORT => {
                                            // Send accumulated mouse movement on sync
                                            if rel_x != 0 || rel_y != 0 {
                                                let event =
                                                    InputEvent::MouseMove(MouseMoveEvent {
                                                        timestamp,
                                                        x: Some(mouse_x),
                                                        y: Some(mouse_y),
                                                        dx: rel_x,
                                                        dy: rel_y,
                                                    });
                                                let _ = tx.blocking_send(event);
                                                rel_x = 0;
                                                rel_y = 0;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                break;
                            }
                            Err(e) => {
                                tracing::warn!("Error reading from {}: {}", path, e);
                                break;
                            }
                        }
                    }
                }

                if !had_event {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }

            tracing::info!("Input capture stopped");
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> InputResult<()> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(InputError::NotStarted);
        }

        self.capturing.store(false, Ordering::SeqCst);

        // Release any grabbed devices
        for fd in &self.grabbed_devices {
            unsafe {
                // EVIOCGRAB with 0 releases the grab
                libc::ioctl(*fd, 0x40044590u64 as libc::c_ulong, 0);
            }
        }
        self.grabbed_devices.clear();

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

        // When suppressing, grab the devices exclusively
        // This prevents events from reaching other applications
        if suppress {
            for path in &self.device_paths {
                if let Ok(file) = OpenOptions::new().read(true).write(true).open(path) {
                    let fd = file.as_raw_fd();
                    unsafe {
                        // EVIOCGRAB grabs the device exclusively
                        if libc::ioctl(fd, 0x40044590u64 as libc::c_ulong, 1) == 0 {
                            self.grabbed_devices.push(fd);
                            std::mem::forget(file); // Keep the file open
                        }
                    }
                }
            }
        } else {
            // Release grabs
            for fd in &self.grabbed_devices {
                unsafe {
                    libc::ioctl(*fd, 0x40044590u64 as libc::c_ulong, 0);
                }
            }
            self.grabbed_devices.clear();
        }
    }

    fn is_suppressing(&self) -> bool {
        self.suppressing.load(Ordering::SeqCst)
    }
}

/// Linux input injection implementation using uinput
pub struct LinuxInputInjector {
    initialized: bool,
    uinput_file: Option<File>,
    screen_width: u32,
    screen_height: u32,
}

impl LinuxInputInjector {
    pub fn new() -> Self {
        Self {
            initialized: false,
            uinput_file: None,
            screen_width: 1920,
            screen_height: 1080,
        }
    }

    pub fn with_screen_size(mut self, width: u32, height: u32) -> Self {
        self.screen_width = width;
        self.screen_height = height;
        self
    }

    /// Check if uinput is available
    pub fn is_uinput_available() -> bool {
        Path::new("/dev/uinput").exists()
    }

    fn write_event(&mut self, type_: u16, code: u16, value: i32) -> InputResult<()> {
        let event = InputEventRaw::new(type_, code, value);
        if let Some(ref mut file) = self.uinput_file {
            file.write_all(&event.to_bytes())
                .map_err(|e| InputError::Io(e))?;
        }
        Ok(())
    }

    fn sync(&mut self) -> InputResult<()> {
        self.write_event(EV_SYN, SYN_REPORT, 0)
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
                "/dev/uinput not found. Load the module: sudo modprobe uinput".to_string(),
            ));
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .map_err(|e| {
                InputError::PermissionDenied(format!(
                    "Cannot open /dev/uinput: {}. Try: sudo chmod 666 /dev/uinput",
                    e
                ))
            })?;

        let fd = file.as_raw_fd();

        unsafe {
            // UI_SET_EVBIT - enable event types
            libc::ioctl(fd, 0x40045564, EV_KEY as libc::c_int); // UI_SET_EVBIT
            libc::ioctl(fd, 0x40045564, EV_REL as libc::c_int);
            libc::ioctl(fd, 0x40045564, EV_ABS as libc::c_int);
            libc::ioctl(fd, 0x40045564, EV_SYN as libc::c_int);

            // UI_SET_KEYBIT - enable keys
            for key in 0..256 {
                libc::ioctl(fd, 0x40045565, key); // UI_SET_KEYBIT
            }

            // Mouse buttons
            for btn in BTN_LEFT..=BTN_EXTRA {
                libc::ioctl(fd, 0x40045565, btn as libc::c_int);
            }

            // UI_SET_RELBIT - enable relative axes
            libc::ioctl(fd, 0x40045566, REL_X as libc::c_int); // UI_SET_RELBIT
            libc::ioctl(fd, 0x40045566, REL_Y as libc::c_int);
            libc::ioctl(fd, 0x40045566, REL_WHEEL as libc::c_int);
            libc::ioctl(fd, 0x40045566, REL_HWHEEL as libc::c_int);

            // UI_SET_ABSBIT - enable absolute axes
            libc::ioctl(fd, 0x40045567, ABS_X as libc::c_int); // UI_SET_ABSBIT
            libc::ioctl(fd, 0x40045567, ABS_Y as libc::c_int);
        }

        // Write device setup
        let mut setup_data = vec![0u8; 92]; // uinput_user_dev size

        // Device name: "CoreNet Virtual Input"
        let name = b"CoreNet Virtual Input";
        setup_data[0..name.len()].copy_from_slice(name);

        // Set bustype, vendor, product, version at offset 80
        setup_data[80..82].copy_from_slice(&3u16.to_ne_bytes()); // BUS_USB
        setup_data[82..84].copy_from_slice(&0x1234u16.to_ne_bytes()); // vendor
        setup_data[84..86].copy_from_slice(&0x5678u16.to_ne_bytes()); // product
        setup_data[86..88].copy_from_slice(&1u16.to_ne_bytes()); // version

        file.write_all(&setup_data)
            .map_err(|e| InputError::Io(e))?;

        // Create the device
        unsafe {
            let ret = libc::ioctl(fd, 0x5501); // UI_DEV_CREATE
            if ret < 0 {
                return Err(InputError::Platform(format!(
                    "UI_DEV_CREATE failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }

        // Wait for device to be ready
        std::thread::sleep(Duration::from_millis(100));

        self.uinput_file = Some(file);
        self.initialized = true;

        tracing::info!("Linux input injector initialized via uinput");
        Ok(())
    }

    async fn shutdown(&mut self) -> InputResult<()> {
        if let Some(ref file) = self.uinput_file {
            unsafe {
                libc::ioctl(file.as_raw_fd(), 0x5502); // UI_DEV_DESTROY
            }
        }
        self.uinput_file = None;
        self.initialized = false;
        Ok(())
    }

    async fn mouse_move_relative(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        if dx != 0 {
            self.write_event(EV_REL, REL_X, dx)?;
        }
        if dy != 0 {
            self.write_event(EV_REL, REL_Y, dy)?;
        }
        self.sync()?;

        Ok(())
    }

    async fn mouse_move_absolute(&mut self, x: i32, y: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        self.write_event(EV_ABS, ABS_X, x)?;
        self.write_event(EV_ABS, ABS_Y, y)?;
        self.sync()?;

        Ok(())
    }

    async fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let btn_code = match button {
            MouseButton::Left => BTN_LEFT,
            MouseButton::Right => BTN_RIGHT,
            MouseButton::Middle => BTN_MIDDLE,
            MouseButton::Button4 => BTN_SIDE,
            MouseButton::Button5 => BTN_EXTRA,
        };

        self.write_event(EV_KEY, btn_code, if pressed { 1 } else { 0 })?;
        self.sync()?;

        Ok(())
    }

    async fn mouse_scroll(&mut self, dx: i32, dy: i32) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        if dy != 0 {
            self.write_event(EV_REL, REL_WHEEL, dy)?;
        }
        if dx != 0 {
            self.write_event(EV_REL, REL_HWHEEL, dx)?;
        }
        self.sync()?;

        Ok(())
    }

    async fn key_down(&mut self, keycode: u32, _modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let linux_keycode = hid_to_linux_keycode(keycode);
        self.write_event(EV_KEY, linux_keycode as u16, 1)?;
        self.sync()?;

        Ok(())
    }

    async fn key_up(&mut self, keycode: u32, _modifiers: Modifiers) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        let linux_keycode = hid_to_linux_keycode(keycode);
        self.write_event(EV_KEY, linux_keycode as u16, 0)?;
        self.sync()?;

        Ok(())
    }

    async fn type_char(&mut self, c: char) -> InputResult<()> {
        if !self.initialized {
            return Err(InputError::NotStarted);
        }

        // Map character to keycode
        if let Some((keycode, shift)) = char_to_keycode(c) {
            if shift {
                self.write_event(EV_KEY, KEY_LEFTSHIFT, 1)?;
                self.sync()?;
            }

            self.write_event(EV_KEY, keycode, 1)?;
            self.sync()?;
            self.write_event(EV_KEY, keycode, 0)?;
            self.sync()?;

            if shift {
                self.write_event(EV_KEY, KEY_LEFTSHIFT, 0)?;
                self.sync()?;
            }
        }

        Ok(())
    }
}

/// Convert USB HID keycode to Linux keycode
fn hid_to_linux_keycode(hid: u32) -> u32 {
    // USB HID to Linux keycode mapping
    static HID_TO_LINUX: &[(u32, u32)] = &[
        (0x04, 30),  // A
        (0x05, 48),  // B
        (0x06, 46),  // C
        (0x07, 32),  // D
        (0x08, 18),  // E
        (0x09, 33),  // F
        (0x0A, 34),  // G
        (0x0B, 35),  // H
        (0x0C, 23),  // I
        (0x0D, 36),  // J
        (0x0E, 37),  // K
        (0x0F, 38),  // L
        (0x10, 50),  // M
        (0x11, 49),  // N
        (0x12, 24),  // O
        (0x13, 25),  // P
        (0x14, 16),  // Q
        (0x15, 19),  // R
        (0x16, 31),  // S
        (0x17, 20),  // T
        (0x18, 22),  // U
        (0x19, 47),  // V
        (0x1A, 17),  // W
        (0x1B, 45),  // X
        (0x1C, 21),  // Y
        (0x1D, 44),  // Z
        (0x1E, 2),   // 1
        (0x1F, 3),   // 2
        (0x20, 4),   // 3
        (0x21, 5),   // 4
        (0x22, 6),   // 5
        (0x23, 7),   // 6
        (0x24, 8),   // 7
        (0x25, 9),   // 8
        (0x26, 10),  // 9
        (0x27, 11),  // 0
        (0x28, 28),  // Enter
        (0x29, 1),   // Escape
        (0x2A, 14),  // Backspace
        (0x2B, 15),  // Tab
        (0x2C, 57),  // Space
        (0x2D, 12),  // Minus
        (0x2E, 13),  // Equal
        (0x2F, 26),  // Left Bracket
        (0x30, 27),  // Right Bracket
        (0x31, 43),  // Backslash
        (0x33, 39),  // Semicolon
        (0x34, 40),  // Quote
        (0x35, 41),  // Grave
        (0x36, 51),  // Comma
        (0x37, 52),  // Period
        (0x38, 53),  // Slash
        (0x39, 58),  // Caps Lock
        (0x3A, 59),  // F1
        (0x3B, 60),  // F2
        (0x3C, 61),  // F3
        (0x3D, 62),  // F4
        (0x3E, 63),  // F5
        (0x3F, 64),  // F6
        (0x40, 65),  // F7
        (0x41, 66),  // F8
        (0x42, 67),  // F9
        (0x43, 68),  // F10
        (0x44, 87),  // F11
        (0x45, 88),  // F12
        (0x4F, 106), // Right Arrow
        (0x50, 105), // Left Arrow
        (0x51, 108), // Down Arrow
        (0x52, 103), // Up Arrow
        (0xE0, 29),  // Left Ctrl
        (0xE1, 42),  // Left Shift
        (0xE2, 56),  // Left Alt
        (0xE3, 125), // Left Meta
        (0xE4, 97),  // Right Ctrl
        (0xE5, 54),  // Right Shift
        (0xE6, 100), // Right Alt
        (0xE7, 126), // Right Meta
    ];

    for &(h, l) in HID_TO_LINUX {
        if h == hid {
            return l;
        }
    }

    0
}

/// Convert Linux keycode to USB HID keycode
fn linux_to_hid_keycode(linux: u32) -> u32 {
    static LINUX_TO_HID: &[(u32, u32)] = &[
        (30, 0x04),  // A
        (48, 0x05),  // B
        (46, 0x06),  // C
        (32, 0x07),  // D
        (18, 0x08),  // E
        (33, 0x09),  // F
        (34, 0x0A),  // G
        (35, 0x0B),  // H
        (23, 0x0C),  // I
        (36, 0x0D),  // J
        (37, 0x0E),  // K
        (38, 0x0F),  // L
        (50, 0x10),  // M
        (49, 0x11),  // N
        (24, 0x12),  // O
        (25, 0x13),  // P
        (16, 0x14),  // Q
        (19, 0x15),  // R
        (31, 0x16),  // S
        (20, 0x17),  // T
        (22, 0x18),  // U
        (47, 0x19),  // V
        (17, 0x1A),  // W
        (45, 0x1B),  // X
        (21, 0x1C),  // Y
        (44, 0x1D),  // Z
        (2, 0x1E),   // 1
        (3, 0x1F),   // 2
        (4, 0x20),   // 3
        (5, 0x21),   // 4
        (6, 0x22),   // 5
        (7, 0x23),   // 6
        (8, 0x24),   // 7
        (9, 0x25),   // 8
        (10, 0x26),  // 9
        (11, 0x27),  // 0
        (28, 0x28),  // Enter
        (1, 0x29),   // Escape
        (14, 0x2A),  // Backspace
        (15, 0x2B),  // Tab
        (57, 0x2C),  // Space
        (12, 0x2D),  // Minus
        (13, 0x2E),  // Equal
        (26, 0x2F),  // Left Bracket
        (27, 0x30),  // Right Bracket
        (43, 0x31),  // Backslash
        (39, 0x33),  // Semicolon
        (40, 0x34),  // Quote
        (41, 0x35),  // Grave
        (51, 0x36),  // Comma
        (52, 0x37),  // Period
        (53, 0x38),  // Slash
        (58, 0x39),  // Caps Lock
        (59, 0x3A),  // F1
        (60, 0x3B),  // F2
        (61, 0x3C),  // F3
        (62, 0x3D),  // F4
        (63, 0x3E),  // F5
        (64, 0x3F),  // F6
        (65, 0x40),  // F7
        (66, 0x41),  // F8
        (67, 0x42),  // F9
        (68, 0x43),  // F10
        (87, 0x44),  // F11
        (88, 0x45),  // F12
        (106, 0x4F), // Right Arrow
        (105, 0x50), // Left Arrow
        (108, 0x51), // Down Arrow
        (103, 0x52), // Up Arrow
        (29, 0xE0),  // Left Ctrl
        (42, 0xE1),  // Left Shift
        (56, 0xE2),  // Left Alt
        (125, 0xE3), // Left Meta
        (97, 0xE4),  // Right Ctrl
        (54, 0xE5),  // Right Shift
        (100, 0xE6), // Right Alt
        (126, 0xE7), // Right Meta
    ];

    for &(l, h) in LINUX_TO_HID {
        if l == linux {
            return h;
        }
    }

    linux // Pass through if not mapped
}

/// Map a character to a Linux keycode and shift state
fn char_to_keycode(c: char) -> Option<(u16, bool)> {
    match c {
        'a'..='z' => Some((c as u16 - 'a' as u16 + 30, false)),
        'A'..='Z' => Some((c as u16 - 'A' as u16 + 30, true)),
        '0' => Some((11, false)),
        '1'..='9' => Some((c as u16 - '1' as u16 + 2, false)),
        ' ' => Some((57, false)),
        '\n' => Some((28, false)),
        '\t' => Some((15, false)),
        '-' => Some((12, false)),
        '=' => Some((13, false)),
        '[' => Some((26, false)),
        ']' => Some((27, false)),
        ';' => Some((39, false)),
        '\'' => Some((40, false)),
        '`' => Some((41, false)),
        '\\' => Some((43, false)),
        ',' => Some((51, false)),
        '.' => Some((52, false)),
        '/' => Some((53, false)),
        '!' => Some((2, true)),
        '@' => Some((3, true)),
        '#' => Some((4, true)),
        '$' => Some((5, true)),
        '%' => Some((6, true)),
        '^' => Some((7, true)),
        '&' => Some((8, true)),
        '*' => Some((9, true)),
        '(' => Some((10, true)),
        ')' => Some((11, true)),
        '_' => Some((12, true)),
        '+' => Some((13, true)),
        '{' => Some((26, true)),
        '}' => Some((27, true)),
        ':' => Some((39, true)),
        '"' => Some((40, true)),
        '~' => Some((41, true)),
        '|' => Some((43, true)),
        '<' => Some((51, true)),
        '>' => Some((52, true)),
        '?' => Some((53, true)),
        _ => None,
    }
}
