//! Screen edge detection
//!
//! Detects when the cursor hits the edge of the screen and determines
//! if it should transition to another host.

use std::time::{Duration, Instant};

use crate::protocol::ScreenEdge;

/// Configuration for edge detection
#[derive(Debug, Clone)]
pub struct EdgeDetectorConfig {
    /// Margin in pixels from the screen edge to trigger detection
    pub edge_margin: u32,
    /// Minimum time cursor must be at edge before transitioning (ms)
    pub dwell_time_ms: u64,
    /// Whether to require double-tap at edge to transition
    pub require_double_tap: bool,
    /// Time window for double-tap detection (ms)
    pub double_tap_window_ms: u64,
    /// Edges that are enabled for transitions
    pub enabled_edges: EdgeMask,
}

impl Default for EdgeDetectorConfig {
    fn default() -> Self {
        Self {
            edge_margin: 1,
            dwell_time_ms: 0, // Instant transition
            require_double_tap: false,
            double_tap_window_ms: 500,
            enabled_edges: EdgeMask::all(),
        }
    }
}

/// Bitmask for enabled edges
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeMask(u8);

impl EdgeMask {
    pub const NONE: EdgeMask = EdgeMask(0);
    pub const LEFT: EdgeMask = EdgeMask(1 << 0);
    pub const RIGHT: EdgeMask = EdgeMask(1 << 1);
    pub const TOP: EdgeMask = EdgeMask(1 << 2);
    pub const BOTTOM: EdgeMask = EdgeMask(1 << 3);

    pub fn all() -> Self {
        Self(0b1111)
    }

    pub fn is_enabled(&self, edge: ScreenEdge) -> bool {
        let bit = match edge {
            ScreenEdge::Left => 1 << 0,
            ScreenEdge::Right => 1 << 1,
            ScreenEdge::Top => 1 << 2,
            ScreenEdge::Bottom => 1 << 3,
        };
        (self.0 & bit) != 0
    }

    pub fn enable(&mut self, edge: ScreenEdge) {
        let bit = match edge {
            ScreenEdge::Left => 1 << 0,
            ScreenEdge::Right => 1 << 1,
            ScreenEdge::Top => 1 << 2,
            ScreenEdge::Bottom => 1 << 3,
        };
        self.0 |= bit;
    }

    pub fn disable(&mut self, edge: ScreenEdge) {
        let bit = match edge {
            ScreenEdge::Left => 1 << 0,
            ScreenEdge::Right => 1 << 1,
            ScreenEdge::Top => 1 << 2,
            ScreenEdge::Bottom => 1 << 3,
        };
        self.0 &= !bit;
    }
}

/// Result of edge detection
#[derive(Debug, Clone)]
pub enum EdgeDetectResult {
    /// Cursor is not at any edge
    NotAtEdge,
    /// Cursor is at edge but waiting for dwell time
    Dwelling {
        edge: ScreenEdge,
        remaining_ms: u64,
    },
    /// Cursor should transition to another screen
    Transition {
        edge: ScreenEdge,
        /// Normalized position along the edge (0.0 to 1.0)
        position: f32,
    },
}

/// State for edge detection
struct EdgeState {
    /// When the cursor first touched this edge
    touch_start: Option<Instant>,
    /// Last time cursor left and returned to edge (for double-tap)
    last_tap: Option<Instant>,
}

impl Default for EdgeState {
    fn default() -> Self {
        Self {
            touch_start: None,
            last_tap: None,
        }
    }
}

/// Detects screen edge transitions
pub struct EdgeDetector {
    /// Configuration
    config: EdgeDetectorConfig,
    /// Screen dimensions
    screen_width: u32,
    screen_height: u32,
    /// State for each edge
    edge_states: [EdgeState; 4],
    /// Currently detected edge (if any)
    current_edge: Option<ScreenEdge>,
}

impl EdgeDetector {
    /// Create a new edge detector
    pub fn new(config: EdgeDetectorConfig, screen_width: u32, screen_height: u32) -> Self {
        Self {
            config,
            screen_width,
            screen_height,
            edge_states: Default::default(),
            current_edge: None,
        }
    }

    /// Update screen dimensions
    pub fn set_screen_size(&mut self, width: u32, height: u32) {
        self.screen_width = width;
        self.screen_height = height;
    }

    /// Check cursor position and detect edge transitions
    pub fn check(&mut self, x: i32, y: i32) -> EdgeDetectResult {
        let margin = self.config.edge_margin as i32;
        let width = self.screen_width as i32;
        let height = self.screen_height as i32;

        // Determine which edge (if any) the cursor is at
        let detected_edge = if x <= margin && self.config.enabled_edges.is_enabled(ScreenEdge::Left)
        {
            Some(ScreenEdge::Left)
        } else if x >= width - margin - 1
            && self.config.enabled_edges.is_enabled(ScreenEdge::Right)
        {
            Some(ScreenEdge::Right)
        } else if y <= margin && self.config.enabled_edges.is_enabled(ScreenEdge::Top) {
            Some(ScreenEdge::Top)
        } else if y >= height - margin - 1
            && self.config.enabled_edges.is_enabled(ScreenEdge::Bottom)
        {
            Some(ScreenEdge::Bottom)
        } else {
            None
        };

        // Handle edge state transitions
        match (self.current_edge, detected_edge) {
            // No longer at edge
            (Some(prev_edge), None) => {
                // Reset state for the previous edge
                let idx = prev_edge as usize;
                
                // Record leaving for double-tap detection
                if self.config.require_double_tap {
                    self.edge_states[idx].last_tap = self.edge_states[idx].touch_start;
                }
                
                self.edge_states[idx].touch_start = None;
                self.current_edge = None;
                
                EdgeDetectResult::NotAtEdge
            }

            // Just reached an edge
            (None, Some(edge)) => {
                let idx = edge as usize;
                let now = Instant::now();

                // Check for double-tap
                if self.config.require_double_tap {
                    if let Some(last_tap) = self.edge_states[idx].last_tap {
                        let elapsed = now.duration_since(last_tap);
                        if elapsed.as_millis() as u64 > self.config.double_tap_window_ms {
                            // Too slow, not a double-tap - start new dwell
                            self.edge_states[idx].touch_start = Some(now);
                            self.edge_states[idx].last_tap = None;
                            self.current_edge = Some(edge);
                            return EdgeDetectResult::Dwelling {
                                edge,
                                remaining_ms: self.config.dwell_time_ms,
                            };
                        } else {
                            // Double-tap detected! Transition immediately
                            self.edge_states[idx].last_tap = None;
                            self.current_edge = Some(edge);
                            return EdgeDetectResult::Transition {
                                edge,
                                position: self.calculate_edge_position(edge, x, y),
                            };
                        }
                    }
                }

                self.edge_states[idx].touch_start = Some(now);
                self.current_edge = Some(edge);

                if self.config.dwell_time_ms == 0 && !self.config.require_double_tap {
                    // Instant transition
                    EdgeDetectResult::Transition {
                        edge,
                        position: self.calculate_edge_position(edge, x, y),
                    }
                } else {
                    EdgeDetectResult::Dwelling {
                        edge,
                        remaining_ms: self.config.dwell_time_ms,
                    }
                }
            }

            // Still at the same edge
            (Some(edge), Some(new_edge)) if edge == new_edge => {
                let idx = edge as usize;
                
                if let Some(start) = self.edge_states[idx].touch_start {
                    let elapsed = start.elapsed().as_millis() as u64;
                    
                    if elapsed >= self.config.dwell_time_ms {
                        // Dwell time complete, transition
                        EdgeDetectResult::Transition {
                            edge,
                            position: self.calculate_edge_position(edge, x, y),
                        }
                    } else {
                        EdgeDetectResult::Dwelling {
                            edge,
                            remaining_ms: self.config.dwell_time_ms - elapsed,
                        }
                    }
                } else {
                    EdgeDetectResult::NotAtEdge
                }
            }

            // Moved directly from one edge to another (corner case)
            (Some(_prev_edge), Some(new_edge)) => {
                // Reset all states and start fresh at new edge
                self.edge_states = Default::default();
                self.current_edge = Some(new_edge);
                
                let idx = new_edge as usize;
                self.edge_states[idx].touch_start = Some(Instant::now());
                
                if self.config.dwell_time_ms == 0 {
                    EdgeDetectResult::Transition {
                        edge: new_edge,
                        position: self.calculate_edge_position(new_edge, x, y),
                    }
                } else {
                    EdgeDetectResult::Dwelling {
                        edge: new_edge,
                        remaining_ms: self.config.dwell_time_ms,
                    }
                }
            }

            // Not at edge, still not at edge
            (None, None) => EdgeDetectResult::NotAtEdge,
        }
    }

    /// Calculate normalized position along an edge
    fn calculate_edge_position(&self, edge: ScreenEdge, x: i32, y: i32) -> f32 {
        match edge {
            ScreenEdge::Left | ScreenEdge::Right => {
                (y as f32 / self.screen_height as f32).clamp(0.0, 1.0)
            }
            ScreenEdge::Top | ScreenEdge::Bottom => {
                (x as f32 / self.screen_width as f32).clamp(0.0, 1.0)
            }
        }
    }

    /// Reset all edge states
    pub fn reset(&mut self) {
        self.edge_states = Default::default();
        self.current_edge = None;
    }

    /// Get the current edge (if cursor is at one)
    pub fn current_edge(&self) -> Option<ScreenEdge> {
        self.current_edge
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_detection_left() {
        let config = EdgeDetectorConfig::default();
        let mut detector = EdgeDetector::new(config, 1920, 1080);
        
        // Move to left edge
        let result = detector.check(0, 500);
        
        match result {
            EdgeDetectResult::Transition { edge, position } => {
                assert_eq!(edge, ScreenEdge::Left);
                assert!(position > 0.4 && position < 0.5);
            }
            _ => panic!("Expected transition"),
        }
    }

    #[test]
    fn test_edge_detection_right() {
        let config = EdgeDetectorConfig::default();
        let mut detector = EdgeDetector::new(config, 1920, 1080);
        
        let result = detector.check(1919, 500);
        
        match result {
            EdgeDetectResult::Transition { edge, .. } => {
                assert_eq!(edge, ScreenEdge::Right);
            }
            _ => panic!("Expected transition"),
        }
    }

    #[test]
    fn test_edge_detection_with_dwell() {
        let config = EdgeDetectorConfig {
            dwell_time_ms: 100,
            ..Default::default()
        };
        let mut detector = EdgeDetector::new(config, 1920, 1080);
        
        // First check should be dwelling
        let result = detector.check(0, 500);
        assert!(matches!(result, EdgeDetectResult::Dwelling { .. }));
        
        // After dwell time, should transition
        std::thread::sleep(Duration::from_millis(150));
        let result = detector.check(0, 500);
        assert!(matches!(result, EdgeDetectResult::Transition { .. }));
    }

    #[test]
    fn test_disabled_edge() {
        let mut config = EdgeDetectorConfig::default();
        config.enabled_edges.disable(ScreenEdge::Left);
        let mut detector = EdgeDetector::new(config, 1920, 1080);
        
        // Left edge should not trigger
        let result = detector.check(0, 500);
        assert!(matches!(result, EdgeDetectResult::NotAtEdge));
        
        // Right edge should still work
        let result = detector.check(1919, 500);
        assert!(matches!(result, EdgeDetectResult::Transition { .. }));
    }
}
