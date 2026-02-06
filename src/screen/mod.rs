//! Screen management module
//!
//! Handles:
//! - Screen edge detection for cursor transitions
//! - Screen layout configuration
//! - Cursor position tracking

mod edge_detector;
mod layout;

pub use edge_detector::{EdgeDetectResult, EdgeDetector, EdgeDetectorConfig, EdgeMask};
pub use layout::{LayoutBuilder, ScreenLayout, ScreenNode};

use crate::protocol::ScreenEdge;

/// Get the screen dimensions for the current platform
#[cfg(target_os = "macos")]
pub fn get_screen_dimensions() -> (u32, u32) {
    // In real implementation:
    // CGDisplayPixelsWide(CGMainDisplayID())
    // CGDisplayPixelsHigh(CGMainDisplayID())
    (2560, 1600) // Default for Retina MacBook Pro
}

#[cfg(target_os = "windows")]
pub fn get_screen_dimensions() -> (u32, u32) {
    // In real implementation:
    // GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)
    (1920, 1080)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn get_screen_dimensions() -> (u32, u32) {
    (1920, 1080)
}

/// Convert cursor position to a normalized edge position (0.0 to 1.0)
pub fn normalize_edge_position(edge: ScreenEdge, x: i32, y: i32, width: u32, height: u32) -> f32 {
    match edge {
        ScreenEdge::Left | ScreenEdge::Right => y as f32 / height as f32,
        ScreenEdge::Top | ScreenEdge::Bottom => x as f32 / width as f32,
    }
}

/// Convert a normalized edge position back to screen coordinates
pub fn denormalize_edge_position(
    edge: ScreenEdge,
    position: f32,
    width: u32,
    height: u32,
) -> (i32, i32) {
    match edge {
        ScreenEdge::Left => (0, (position * height as f32) as i32),
        ScreenEdge::Right => ((width - 1) as i32, (position * height as f32) as i32),
        ScreenEdge::Top => ((position * width as f32) as i32, 0),
        ScreenEdge::Bottom => ((position * width as f32) as i32, (height - 1) as i32),
    }
}

/// Get the opposite edge (for screen transitions)
pub fn opposite_edge(edge: ScreenEdge) -> ScreenEdge {
    match edge {
        ScreenEdge::Left => ScreenEdge::Right,
        ScreenEdge::Right => ScreenEdge::Left,
        ScreenEdge::Top => ScreenEdge::Bottom,
        ScreenEdge::Bottom => ScreenEdge::Top,
    }
}
