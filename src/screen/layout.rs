//! Screen layout management
//!
//! Manages the logical arrangement of screens across multiple hosts.

use std::collections::HashMap;

use crate::protocol::{ScreenEdge, ScreenInfo};

/// A screen in the layout
#[derive(Debug, Clone)]
pub struct ScreenNode {
    /// Unique host identifier
    pub host_id: String,
    /// Human-readable name
    pub host_name: String,
    /// Screen dimensions
    pub width: u32,
    pub height: u32,
    /// Neighboring hosts by edge
    pub neighbors: HashMap<ScreenEdge, String>,
}

impl ScreenNode {
    pub fn new(info: &ScreenInfo) -> Self {
        Self {
            host_id: info.host_id.clone(),
            host_name: info.host_name.clone(),
            width: info.width,
            height: info.height,
            neighbors: HashMap::new(),
        }
    }

    pub fn set_neighbor(&mut self, edge: ScreenEdge, host_id: String) {
        self.neighbors.insert(edge, host_id);
    }

    pub fn get_neighbor(&self, edge: ScreenEdge) -> Option<&String> {
        self.neighbors.get(&edge)
    }

    pub fn remove_neighbor(&mut self, edge: ScreenEdge) {
        self.neighbors.remove(&edge);
    }
}

/// Manages the layout of screens across hosts
#[derive(Debug, Default)]
pub struct ScreenLayout {
    /// All screens in the layout
    screens: HashMap<String, ScreenNode>,
    /// The local host's ID
    local_host_id: Option<String>,
}

impl ScreenLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the local host
    pub fn set_local_host(&mut self, info: &ScreenInfo) {
        self.local_host_id = Some(info.host_id.clone());
        self.screens.insert(info.host_id.clone(), ScreenNode::new(info));
    }

    /// Add a remote host to the layout
    pub fn add_host(&mut self, info: &ScreenInfo) {
        self.screens.insert(info.host_id.clone(), ScreenNode::new(info));
    }

    /// Remove a host from the layout
    pub fn remove_host(&mut self, host_id: &str) {
        self.screens.remove(host_id);
        
        // Remove references from other hosts
        for screen in self.screens.values_mut() {
            screen.neighbors.retain(|_, id| id != host_id);
        }
    }

    /// Get a host by ID
    pub fn get_host(&self, host_id: &str) -> Option<&ScreenNode> {
        self.screens.get(host_id)
    }

    /// Get mutable reference to a host
    pub fn get_host_mut(&mut self, host_id: &str) -> Option<&mut ScreenNode> {
        self.screens.get_mut(host_id)
    }

    /// Get the local host
    pub fn local_host(&self) -> Option<&ScreenNode> {
        self.local_host_id.as_ref().and_then(|id| self.screens.get(id))
    }

    /// Connect two hosts at specified edges
    pub fn connect_hosts(
        &mut self,
        host_a: &str,
        edge_a: ScreenEdge,
        host_b: &str,
        edge_b: ScreenEdge,
    ) -> bool {
        // Verify both hosts exist
        if !self.screens.contains_key(host_a) || !self.screens.contains_key(host_b) {
            return false;
        }

        // Set up bidirectional connection
        if let Some(screen) = self.screens.get_mut(host_a) {
            screen.set_neighbor(edge_a, host_b.to_string());
        }
        
        if let Some(screen) = self.screens.get_mut(host_b) {
            screen.set_neighbor(edge_b, host_a.to_string());
        }

        true
    }

    /// Disconnect two hosts
    pub fn disconnect_hosts(&mut self, host_a: &str, host_b: &str) {
        if let Some(screen) = self.screens.get_mut(host_a) {
            screen.neighbors.retain(|_, id| id != host_b);
        }
        
        if let Some(screen) = self.screens.get_mut(host_b) {
            screen.neighbors.retain(|_, id| id != host_a);
        }
    }

    /// Get the neighbor of a host at a specific edge
    pub fn get_neighbor(&self, host_id: &str, edge: ScreenEdge) -> Option<&ScreenNode> {
        self.screens
            .get(host_id)
            .and_then(|s| s.get_neighbor(edge))
            .and_then(|id| self.screens.get(id))
    }

    /// Get all hosts in the layout
    pub fn all_hosts(&self) -> impl Iterator<Item = &ScreenNode> {
        self.screens.values()
    }

    /// Get the number of hosts
    pub fn host_count(&self) -> usize {
        self.screens.len()
    }

    /// Create a simple left-right layout
    pub fn create_linear_layout(&mut self, hosts: &[ScreenInfo]) {
        // Clear existing
        self.screens.clear();
        
        // Add all hosts
        for info in hosts {
            self.add_host(info);
        }

        // Connect in sequence
        for i in 0..hosts.len().saturating_sub(1) {
            self.connect_hosts(
                &hosts[i].host_id,
                ScreenEdge::Right,
                &hosts[i + 1].host_id,
                ScreenEdge::Left,
            );
        }
    }
}

/// Builder for creating screen layouts
pub struct LayoutBuilder {
    layout: ScreenLayout,
}

impl LayoutBuilder {
    pub fn new() -> Self {
        Self {
            layout: ScreenLayout::new(),
        }
    }

    pub fn local_host(mut self, info: &ScreenInfo) -> Self {
        self.layout.set_local_host(info);
        self
    }

    pub fn add_host(mut self, info: &ScreenInfo) -> Self {
        self.layout.add_host(info);
        self
    }

    pub fn connect(
        mut self,
        host_a: &str,
        edge_a: ScreenEdge,
        host_b: &str,
        edge_b: ScreenEdge,
    ) -> Self {
        self.layout.connect_hosts(host_a, edge_a, host_b, edge_b);
        self
    }

    /// Connect host_b to the left of host_a
    pub fn left_of(self, host_a: &str, host_b: &str) -> Self {
        self.connect(host_a, ScreenEdge::Left, host_b, ScreenEdge::Right)
    }

    /// Connect host_b to the right of host_a
    pub fn right_of(self, host_a: &str, host_b: &str) -> Self {
        self.connect(host_a, ScreenEdge::Right, host_b, ScreenEdge::Left)
    }

    /// Connect host_b above host_a
    pub fn above(self, host_a: &str, host_b: &str) -> Self {
        self.connect(host_a, ScreenEdge::Top, host_b, ScreenEdge::Bottom)
    }

    /// Connect host_b below host_a
    pub fn below(self, host_a: &str, host_b: &str) -> Self {
        self.connect(host_a, ScreenEdge::Bottom, host_b, ScreenEdge::Top)
    }

    pub fn build(self) -> ScreenLayout {
        self.layout
    }
}

impl Default for LayoutBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_screen_info(id: &str, name: &str) -> ScreenInfo {
        ScreenInfo::new(id.to_string(), name.to_string(), 1920, 1080)
    }

    #[test]
    fn test_layout_creation() {
        let local = make_screen_info("local", "Local Machine");
        let remote = make_screen_info("remote", "Remote Machine");

        let layout = LayoutBuilder::new()
            .local_host(&local)
            .add_host(&remote)
            .right_of("local", "remote")
            .build();

        assert_eq!(layout.host_count(), 2);
        
        let neighbor = layout.get_neighbor("local", ScreenEdge::Right);
        assert!(neighbor.is_some());
        assert_eq!(neighbor.unwrap().host_id, "remote");
    }

    #[test]
    fn test_linear_layout() {
        let hosts = vec![
            make_screen_info("a", "Host A"),
            make_screen_info("b", "Host B"),
            make_screen_info("c", "Host C"),
        ];

        let mut layout = ScreenLayout::new();
        layout.create_linear_layout(&hosts);

        // A -> B
        assert_eq!(
            layout.get_neighbor("a", ScreenEdge::Right).map(|n| &n.host_id),
            Some(&"b".to_string())
        );
        
        // B -> C
        assert_eq!(
            layout.get_neighbor("b", ScreenEdge::Right).map(|n| &n.host_id),
            Some(&"c".to_string())
        );
        
        // B <- C
        assert_eq!(
            layout.get_neighbor("c", ScreenEdge::Left).map(|n| &n.host_id),
            Some(&"b".to_string())
        );
    }
}
