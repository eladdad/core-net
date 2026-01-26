//! Input trait definitions
//!
//! Defines the common interface that platform-specific implementations must provide.

use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;

use super::events::{InputEvent, MouseState, KeyboardState};
use crate::protocol::{MouseButton, Modifiers};

/// Errors that can occur during input operations
#[derive(Error, Debug)]
pub enum InputError {
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    
    #[error("Device not found: {0}")]
    DeviceNotFound(String),
    
    #[error("Platform error: {0}")]
    Platform(String),
    
    #[error("Not supported on this platform")]
    NotSupported,
    
    #[error("Input capture already started")]
    AlreadyStarted,
    
    #[error("Input capture not started")]
    NotStarted,
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type InputResult<T> = Result<T, InputError>;

/// Trait for capturing input events from the local system
#[async_trait]
pub trait InputCapture: Send + Sync {
    /// Start capturing input events
    /// Returns a receiver that will emit captured events
    async fn start(&mut self) -> InputResult<mpsc::Receiver<InputEvent>>;
    
    /// Stop capturing input events
    async fn stop(&mut self) -> InputResult<()>;
    
    /// Check if capture is currently active
    fn is_capturing(&self) -> bool;
    
    /// Get the current mouse state
    fn mouse_state(&self) -> MouseState;
    
    /// Get the current keyboard state
    fn keyboard_state(&self) -> KeyboardState;
    
    /// Set whether captured events should be suppressed (not passed to local system)
    fn set_suppress(&mut self, suppress: bool);
    
    /// Check if events are being suppressed
    fn is_suppressing(&self) -> bool;
}

/// Trait for injecting input events into the local system
#[async_trait]
pub trait InputInjector: Send + Sync {
    /// Initialize the injector
    async fn init(&mut self) -> InputResult<()>;
    
    /// Shutdown the injector
    async fn shutdown(&mut self) -> InputResult<()>;
    
    /// Move the mouse by a relative amount
    async fn mouse_move_relative(&mut self, dx: i32, dy: i32) -> InputResult<()>;
    
    /// Move the mouse to an absolute position
    async fn mouse_move_absolute(&mut self, x: i32, y: i32) -> InputResult<()>;
    
    /// Press or release a mouse button
    async fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> InputResult<()>;
    
    /// Scroll the mouse wheel
    async fn mouse_scroll(&mut self, dx: i32, dy: i32) -> InputResult<()>;
    
    /// Press a key
    async fn key_down(&mut self, keycode: u32, modifiers: Modifiers) -> InputResult<()>;
    
    /// Release a key
    async fn key_up(&mut self, keycode: u32, modifiers: Modifiers) -> InputResult<()>;
    
    /// Type a character (handles shift automatically)
    async fn type_char(&mut self, c: char) -> InputResult<()>;
    
    /// Type a string
    async fn type_string(&mut self, s: &str) -> InputResult<()> {
        for c in s.chars() {
            self.type_char(c).await?;
        }
        Ok(())
    }
}

/// Factory function type for creating platform-specific capture instances
pub type CaptureFactory = fn() -> Box<dyn InputCapture>;

/// Factory function type for creating platform-specific injector instances
pub type InjectorFactory = fn() -> Box<dyn InputInjector>;

/// Callback type for input events
pub type InputCallback = Arc<dyn Fn(InputEvent) + Send + Sync>;
