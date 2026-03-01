use std::collections::HashMap;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

/// A labeled, weighted edge between two memory nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub from: u32,
    pub to: u32,
    pub label: String,
    pub weight: f32,
}

/// Bidirectional memory graph using roaring bitmaps for adjacency.
#[derive(Debug, Default)]
pub struct MemoryGraph {
    adjacency: HashMap<u32, RoaringBitmap>,
    edges: HashMap<(u32, u32), Edge>,
}

impl MemoryGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect two memories with a labeled, weighted edge.
    pub fn connect(&mut self, a: u32, b: u32, label: String, weight: f32) {
        self.adjacency.entry(a).or_default().insert(b);
        self.adjacency.entry(b).or_default().insert(a);
        let key = canonical_key(a, b);
        self.edges.insert(
            key,
            Edge {
                from: a,
                to: b,
                label,
                weight,
            },
        );
    }

    /// Remove the edge between two memories.
    pub fn disconnect(&mut self, a: u32, b: u32) {
        if let Some(bm) = self.adjacency.get_mut(&a) {
            bm.remove(b);
            if bm.is_empty() {
                self.adjacency.remove(&a);
            }
        }
        if let Some(bm) = self.adjacency.get_mut(&b) {
            bm.remove(a);
            if bm.is_empty() {
                self.adjacency.remove(&b);
            }
        }
        self.edges.remove(&canonical_key(a, b));
    }

    /// Direct neighbors of a node.
    pub fn neighbors(&self, id: u32) -> RoaringBitmap {
        self.adjacency.get(&id).cloned().unwrap_or_default()
    }

    /// The edge between two nodes, if any.
    pub fn edge(&self, a: u32, b: u32) -> Option<&Edge> {
        self.edges.get(&canonical_key(a, b))
    }

    /// Degree centrality (number of direct connections).
    pub fn degree(&self, id: u32) -> u64 {
        self.adjacency.get(&id).map_or(0, |bm| bm.len())
    }

    /// Remove a node and all its edges.
    pub fn remove_node(&mut self, id: u32) {
        if let Some(neighbors) = self.adjacency.remove(&id) {
            for neighbor in neighbors.iter() {
                if let Some(bm) = self.adjacency.get_mut(&neighbor) {
                    bm.remove(id);
                    if bm.is_empty() {
                        self.adjacency.remove(&neighbor);
                    }
                }
                self.edges.remove(&canonical_key(id, neighbor));
            }
        }
    }

    /// Nodes that have at least one edge.
    pub fn node_count(&self) -> usize {
        self.adjacency.len()
    }

    /// Total edges.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Two-hop neighborhood: neighbors of neighbors, excluding self.
    pub fn neighborhood_2(&self, id: u32) -> RoaringBitmap {
        let direct = self.neighbors(id);
        let mut result = direct.clone();
        for neighbor in direct.iter() {
            result |= &self.neighbors(neighbor);
        }
        result.remove(id);
        result
    }

    /// Snapshot all edges for persistence.
    pub fn all_edges(&self) -> Vec<Edge> {
        self.edges.values().cloned().collect()
    }

    /// Rebuild from persisted edges.
    pub fn load_edges(&mut self, edges: Vec<Edge>) {
        for e in edges {
            self.connect(e.from, e.to, e.label.clone(), e.weight);
        }
    }
}

fn canonical_key(a: u32, b: u32) -> (u32, u32) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_and_neighbors() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "related".into(), 1.0);
        g.connect(1, 3, "caused".into(), 0.8);

        let n = g.neighbors(1);
        assert!(n.contains(2));
        assert!(n.contains(3));
        assert_eq!(n.len(), 2);

        // Bidirectional
        assert!(g.neighbors(2).contains(1));
    }

    #[test]
    fn disconnect_removes_edge() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "x".into(), 1.0);
        assert_eq!(g.edge_count(), 1);

        g.disconnect(1, 2);
        assert_eq!(g.edge_count(), 0);
        assert!(g.neighbors(1).is_empty());
        assert!(g.neighbors(2).is_empty());
    }

    #[test]
    fn remove_node_cascades() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0);
        g.connect(1, 3, "b".into(), 1.0);
        g.connect(2, 3, "c".into(), 1.0);

        g.remove_node(1);
        assert_eq!(g.neighbors(1).len(), 0);
        assert!(!g.neighbors(2).contains(1));
        assert!(!g.neighbors(3).contains(1));
        // Edge between 2 and 3 survives
        assert!(g.neighbors(2).contains(3));
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn degree_centrality() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0);
        g.connect(1, 3, "b".into(), 1.0);
        g.connect(1, 4, "c".into(), 1.0);

        assert_eq!(g.degree(1), 3);
        assert_eq!(g.degree(2), 1);
    }

    #[test]
    fn two_hop_neighborhood() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0);
        g.connect(2, 3, "b".into(), 1.0);
        g.connect(3, 4, "c".into(), 1.0);

        let hood = g.neighborhood_2(1);
        assert!(hood.contains(2)); // 1-hop
        assert!(hood.contains(3)); // 2-hop
        assert!(!hood.contains(4)); // 3-hop, out of range
        assert!(!hood.contains(1)); // self excluded
    }

    #[test]
    fn edge_lookup() {
        let mut g = MemoryGraph::new();
        g.connect(5, 3, "link".into(), 0.7);

        // Canonical key should work regardless of arg order
        let e = g.edge(3, 5).unwrap();
        assert_eq!(e.label, "link");
        assert!((e.weight - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn persistence_round_trip() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0);
        g.connect(3, 4, "b".into(), 0.5);

        let edges = g.all_edges();
        let mut g2 = MemoryGraph::new();
        g2.load_edges(edges);

        assert_eq!(g2.edge_count(), 2);
        assert!(g2.neighbors(1).contains(2));
        assert!(g2.neighbors(3).contains(4));
    }
}
