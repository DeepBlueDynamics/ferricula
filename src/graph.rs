use std::collections::HashMap;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

/// Edge directionality — enforces the arrow of time.
///
/// Causal edges implement the DAG constraint from causal set theory:
/// the `to` node cannot traverse backward to the `from` node.
/// This prevents the recursive feedback loops (Upādāna) that would
/// spike thermodynamic heat on a node by re-entering its own past states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    /// Bidirectional — associative link. Both endpoints see each other.
    Semantic,
    /// Directed — causal link. Only `from` sees `to` in traversal.
    Causal,
}

impl Default for EdgeKind {
    fn default() -> Self {
        Self::Semantic
    }
}

/// A labeled, weighted edge between two memory nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub from: u32,
    pub to: u32,
    pub label: String,
    pub weight: f32,
    pub kind: EdgeKind,
}

/// Memory graph with both semantic (bidirectional) and causal (directed) edges.
///
/// Semantic edges populate adjacency in both directions — associative traversal.
/// Causal edges only populate `from → to` in adjacency — the `to` node cannot
/// see the `from` node via `neighbors()`, enforcing the arrow of time.
/// A separate `causal_predecessors` map exists for cleanup only (not traversal).
#[derive(Debug, Default)]
pub struct MemoryGraph {
    /// Forward adjacency: nodes reachable from each node.
    adjacency: HashMap<u32, RoaringBitmap>,
    /// Reverse index for causal edges (cleanup only, not exposed to traversal).
    causal_predecessors: HashMap<u32, RoaringBitmap>,
    edges: HashMap<(u32, u32), Edge>,
}

impl MemoryGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect two memories with a labeled, weighted edge.
    ///
    /// For `Semantic` edges, both nodes see each other in `neighbors()`.
    /// For `Causal` edges, only `from` sees `to` — the arrow of time.
    pub fn connect(&mut self, from: u32, to: u32, label: String, weight: f32, kind: EdgeKind) {
        self.adjacency.entry(from).or_default().insert(to);
        match kind {
            EdgeKind::Semantic => {
                self.adjacency.entry(to).or_default().insert(from);
            }
            EdgeKind::Causal => {
                self.causal_predecessors.entry(to).or_default().insert(from);
            }
        }
        let key = canonical_key(from, to);
        self.edges.insert(
            key,
            Edge {
                from,
                to,
                label,
                weight,
                kind,
            },
        );
    }

    /// Remove the edge between two memories.
    pub fn disconnect(&mut self, a: u32, b: u32) {
        let key = canonical_key(a, b);
        if let Some(edge) = self.edges.remove(&key) {
            // Remove forward adjacency: from → to
            if let Some(bm) = self.adjacency.get_mut(&edge.from) {
                bm.remove(edge.to);
                if bm.is_empty() {
                    self.adjacency.remove(&edge.from);
                }
            }
            match edge.kind {
                EdgeKind::Semantic => {
                    // Remove reverse adjacency: to → from
                    if let Some(bm) = self.adjacency.get_mut(&edge.to) {
                        bm.remove(edge.from);
                        if bm.is_empty() {
                            self.adjacency.remove(&edge.to);
                        }
                    }
                }
                EdgeKind::Causal => {
                    // Remove from causal predecessors index
                    if let Some(bm) = self.causal_predecessors.get_mut(&edge.to) {
                        bm.remove(edge.from);
                        if bm.is_empty() {
                            self.causal_predecessors.remove(&edge.to);
                        }
                    }
                }
            }
        }
    }

    /// Forward-reachable neighbors of a node.
    ///
    /// Semantic edges: both directions visible.
    /// Causal edges: only downstream (`from → to`) visible.
    /// The `to` node of a causal edge cannot see its `from` node here.
    pub fn neighbors(&self, id: u32) -> RoaringBitmap {
        self.adjacency.get(&id).cloned().unwrap_or_default()
    }

    /// Causal predecessors — nodes with causal edges pointing TO this node.
    /// Not included in `neighbors()`. Used for inspection, not traversal.
    pub fn predecessors(&self, id: u32) -> RoaringBitmap {
        self.causal_predecessors
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    /// The edge between two nodes, if any.
    pub fn edge(&self, a: u32, b: u32) -> Option<&Edge> {
        self.edges.get(&canonical_key(a, b))
    }

    /// Degree centrality (number of forward connections).
    pub fn degree(&self, id: u32) -> u64 {
        self.adjacency.get(&id).map_or(0, |bm| bm.len())
    }

    /// Remove a node and all its edges.
    pub fn remove_node(&mut self, id: u32) {
        // Collect all edge keys involving this node
        let to_remove: Vec<(u32, u32)> = self
            .edges
            .keys()
            .filter(|&&(a, b)| a == id || b == id)
            .copied()
            .collect();

        for (a, b) in to_remove {
            self.disconnect(a, b);
        }

        // Clean up any remaining empty entries
        self.adjacency.remove(&id);
        self.causal_predecessors.remove(&id);
    }

    /// Nodes that have at least one edge.
    pub fn node_count(&self) -> usize {
        let mut count = self.adjacency.len();
        // Nodes that only appear as causal targets (no outgoing edges)
        for id in self.causal_predecessors.keys() {
            if !self.adjacency.contains_key(id) {
                count += 1;
            }
        }
        count
    }

    /// Total edges.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Two-hop neighborhood: neighbors of neighbors, excluding self.
    /// Respects causal direction — only traverses forward.
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
            self.connect(e.from, e.to, e.label.clone(), e.weight, e.kind);
        }
    }
}

fn canonical_key(a: u32, b: u32) -> (u32, u32) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_connect_and_neighbors() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "related".into(), 1.0, EdgeKind::Semantic);
        g.connect(1, 3, "caused".into(), 0.8, EdgeKind::Semantic);

        let n = g.neighbors(1);
        assert!(n.contains(2));
        assert!(n.contains(3));
        assert_eq!(n.len(), 2);

        // Bidirectional
        assert!(g.neighbors(2).contains(1));
    }

    #[test]
    fn causal_edge_is_one_way() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "caused".into(), 1.0, EdgeKind::Causal);

        // from (1) can see to (2)
        assert!(g.neighbors(1).contains(2));

        // to (2) CANNOT see from (1) — arrow of time
        assert!(!g.neighbors(2).contains(1));

        // But 2 has 1 as a predecessor (for inspection, not traversal)
        assert!(g.predecessors(2).contains(1));
        assert!(g.predecessors(1).is_empty());
    }

    #[test]
    fn disconnect_semantic() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "x".into(), 1.0, EdgeKind::Semantic);
        assert_eq!(g.edge_count(), 1);

        g.disconnect(1, 2);
        assert_eq!(g.edge_count(), 0);
        assert!(g.neighbors(1).is_empty());
        assert!(g.neighbors(2).is_empty());
    }

    #[test]
    fn disconnect_causal() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "caused".into(), 1.0, EdgeKind::Causal);

        g.disconnect(1, 2);
        assert_eq!(g.edge_count(), 0);
        assert!(g.neighbors(1).is_empty());
        assert!(g.predecessors(2).is_empty());
    }

    #[test]
    fn remove_node_cascades() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0, EdgeKind::Semantic);
        g.connect(1, 3, "b".into(), 1.0, EdgeKind::Causal);
        g.connect(2, 3, "c".into(), 1.0, EdgeKind::Semantic);

        g.remove_node(1);
        assert_eq!(g.neighbors(1).len(), 0);
        assert!(!g.neighbors(2).contains(1));
        assert!(g.predecessors(3).is_empty()); // causal predecessor 1 removed
        // Edge between 2 and 3 survives
        assert!(g.neighbors(2).contains(3));
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn degree_centrality() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0, EdgeKind::Semantic);
        g.connect(1, 3, "b".into(), 1.0, EdgeKind::Semantic);
        g.connect(1, 4, "c".into(), 1.0, EdgeKind::Causal);

        assert_eq!(g.degree(1), 3); // all three forward
        assert_eq!(g.degree(2), 1); // semantic back to 1
        assert_eq!(g.degree(4), 0); // causal target — no forward edges
    }

    #[test]
    fn two_hop_respects_direction() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0, EdgeKind::Causal);
        g.connect(2, 3, "b".into(), 1.0, EdgeKind::Causal);
        g.connect(3, 4, "c".into(), 1.0, EdgeKind::Causal);

        let hood = g.neighborhood_2(1);
        assert!(hood.contains(2)); // 1-hop forward
        assert!(hood.contains(3)); // 2-hop forward
        assert!(!hood.contains(4)); // 3-hop, out of range
        assert!(!hood.contains(1)); // self excluded

        // Node 3 can NOT see backward to 1
        let hood3 = g.neighborhood_2(3);
        assert!(hood3.contains(4)); // forward
        assert!(!hood3.contains(2)); // causal predecessor — invisible
        assert!(!hood3.contains(1)); // causal predecessor — invisible
    }

    #[test]
    fn edge_lookup() {
        let mut g = MemoryGraph::new();
        g.connect(5, 3, "link".into(), 0.7, EdgeKind::Semantic);

        // Canonical key should work regardless of arg order
        let e = g.edge(3, 5).unwrap();
        assert_eq!(e.label, "link");
        assert!((e.weight - 0.7).abs() < f32::EPSILON);
        assert_eq!(e.kind, EdgeKind::Semantic);
    }

    #[test]
    fn causal_edge_preserves_direction_in_edge() {
        let mut g = MemoryGraph::new();
        g.connect(5, 3, "caused".into(), 1.0, EdgeKind::Causal);

        let e = g.edge(3, 5).unwrap();
        assert_eq!(e.from, 5);
        assert_eq!(e.to, 3);
        assert_eq!(e.kind, EdgeKind::Causal);
    }

    #[test]
    fn persistence_round_trip() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0, EdgeKind::Semantic);
        g.connect(3, 4, "b".into(), 0.5, EdgeKind::Causal);

        let edges = g.all_edges();
        let mut g2 = MemoryGraph::new();
        g2.load_edges(edges);

        assert_eq!(g2.edge_count(), 2);
        assert!(g2.neighbors(1).contains(2));
        assert!(g2.neighbors(2).contains(1)); // semantic — bidirectional
        assert!(g2.neighbors(3).contains(4));
        assert!(!g2.neighbors(4).contains(3)); // causal — one-way
        assert!(g2.predecessors(4).contains(3));
    }

    #[test]
    fn mixed_edges_on_same_nodes() {
        // Can't have two edges between the same pair (canonical key collision).
        // The second connect overwrites the first.
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "semantic".into(), 1.0, EdgeKind::Semantic);
        assert!(g.neighbors(2).contains(1)); // bidirectional

        // Overwrite with causal — should update adjacency
        g.connect(1, 2, "caused".into(), 1.0, EdgeKind::Causal);
        let e = g.edge(1, 2).unwrap();
        assert_eq!(e.kind, EdgeKind::Causal);
    }

    #[test]
    fn node_count_includes_causal_targets() {
        let mut g = MemoryGraph::new();
        g.connect(1, 2, "a".into(), 1.0, EdgeKind::Causal);
        // Node 2 only in causal_predecessors, not adjacency
        assert!(g.node_count() >= 2);
    }
}
