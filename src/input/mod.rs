//! Input module - Platform-specific input capture and injection
//!
//! This module provides abstractions for:
//! - Capturing input events (mouse, keyboard)
//! - Injecting input events (for remote control)
//! - Managing input device state

mod events;
mod traits;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "windows")]
mod windows;

// Re-export common types
pub use events::*;
pub use traits::*;

// Re-export platform-specific implementations
#[cfg(target_os = "macos")]
pub use macos::{MacOSInputCapture, MacOSInputInjector};

#[cfg(target_os = "linux")]
pub use linux::{LinuxInputCapture, LinuxInputInjector};

#[cfg(target_os = "windows")]
pub use windows::{WindowsInputCapture, WindowsInputInjector};

/// Get the current platform name
pub fn platform_name() -> &'static str {
    #[cfg(target_os = "macos")]
    return "macOS";
    
    #[cfg(target_os = "linux")]
    return "Linux";
    
    #[cfg(target_os = "windows")]
    return "Windows";
    
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return "Unknown";
}
