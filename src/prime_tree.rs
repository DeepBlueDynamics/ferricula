use std::collections::HashMap;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

/// First 16 primes used as partition thresholds at each tree depth.
const PRIMES: [u64; 16] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53];

/// A node in the prime-partitioned term hierarchy.
/// Each node owns a set of memory IDs. When the set exceeds the node's
/// prime, it splits into child partitions keyed by `member_id % child_prime`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimeNode {
    pub id: u64,
    pub prime: u64,
    pub term: String,
    /// Memory record IDs that belong to this partition.
    pub members: RoaringBitmap,
    pub children: Vec<u64>,
    pub depth: u32,
}

/// Hierarchical prime-partitioned term tree.
/// Port of SlothANN's concept — NOT a vector ANN, but a term graph with
/// prime-based partitioning. Adds the merge operation SlothANN was missing.
#[derive(Debug, Default)]
pub struct PrimeTree {
    nodes: HashMap<u64, PrimeNode>,
    root_ids: Vec<u64>,
    next_id: u64,
}

impl PrimeTree {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            root_ids: Vec::new(),
            next_id: 1,
        }
    }

    /// Insert a memory into the partition for `term`. Creates the node if needed.
    /// Returns the root node ID for that term.
    pub fn insert(&mut self, term: &str, memory_id: u32) -> u64 {
        let node_id = match self.find_root_for_term(term) {
            Some(id) => id,
            None => {
                let id = self.alloc_id();
                let depth = 0;
                let prime = PRIMES[depth as usize % PRIMES.len()];
                self.nodes.insert(
                    id,
                    PrimeNode {
                        id,
                        prime,
                        term: term.to_string(),
                        members: RoaringBitmap::new(),
                        children: Vec::new(),
                        depth,
                    },
                );
                self.root_ids.push(id);
                id
            }
        };

        // Insert into the deepest applicable leaf
        self.insert_into(node_id, memory_id);

        // Auto-split if leaf population exceeds its prime
        self.maybe_split(node_id);

        node_id
    }

    /// Remove a memory ID from every node in the tree.
    pub fn remove_member(&mut self, memory_id: u32) {
        for node in self.nodes.values_mut() {
            node.members.remove(memory_id);
        }
        self.prune_empty_leaves();
    }

    /// Split a node when its member count exceeds its prime.
    /// Members are distributed into children keyed by `member_id % child_prime`.
    pub fn split(&mut self, node_id: u64) {
        let Some(node) = self.nodes.get(&node_id) else {
            return;
        };
        if node.members.len() <= 1 || !node.children.is_empty() {
            return;
        }

        let child_depth = node.depth + 1;
        let child_prime = PRIMES[child_depth as usize % PRIMES.len()];
        let term = node.term.clone();
        let members: Vec<u32> = node.members.iter().collect();

        let mut buckets: HashMap<u64, Vec<u32>> = HashMap::new();
        for &mid in &members {
            let bucket = (mid as u64) % child_prime;
            buckets.entry(bucket).or_default().push(mid);
        }

        if buckets.len() <= 1 {
            return;
        }

        let mut child_ids = Vec::new();
        for (bucket, mids) in buckets {
            let child_id = self.alloc_id();
            let mut child_members = RoaringBitmap::new();
            for mid in mids {
                child_members.insert(mid);
            }
            self.nodes.insert(
                child_id,
                PrimeNode {
                    id: child_id,
                    prime: child_prime,
                    term: format!("{}:{}", term, bucket),
                    members: child_members,
                    children: Vec::new(),
                    depth: child_depth,
                },
            );
            child_ids.push(child_id);
        }

        if let Some(parent) = self.nodes.get_mut(&node_id) {
            parent.members = RoaringBitmap::new();
            parent.children = child_ids;
        }
    }

    /// Merge: the operation SlothANN was missing.
    /// Collapses all descendants back into the parent node.
    pub fn merge(&mut self, node_id: u64) {
        let members = self.collect_members(node_id);
        let child_ids = self.collect_descendant_ids(node_id);

        for cid in child_ids {
            self.nodes.remove(&cid);
        }

        if let Some(node) = self.nodes.get_mut(&node_id) {
            node.members = members;
            node.children.clear();
        }
    }

    /// Search for all memory IDs associated with a term (prefix match).
    pub fn search(&self, term: &str) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for &root_id in &self.root_ids {
            if let Some(root) = self.nodes.get(&root_id) {
                if root.term == term || root.term.starts_with(&format!("{}:", term)) {
                    result |= &self.collect_members(root_id);
                }
            }
        }
        // Also search non-root nodes for prefix matches
        for node in self.nodes.values() {
            if node.term.starts_with(term) && !result.is_empty() {
                result |= &self.collect_members(node.id);
            }
        }
        result
    }

    /// Exact term search — only matches root-level terms.
    pub fn search_exact(&self, term: &str) -> RoaringBitmap {
        match self.find_root_for_term(term) {
            Some(id) => self.collect_members(id),
            None => RoaringBitmap::new(),
        }
    }

    /// Collect all member IDs from a node and its descendants.
    pub fn collect_members(&self, node_id: u64) -> RoaringBitmap {
        let Some(node) = self.nodes.get(&node_id) else {
            return RoaringBitmap::new();
        };
        let mut result = node.members.clone();
        for &child_id in &node.children {
            result |= &self.collect_members(child_id);
        }
        result
    }

    /// All term strings at root level.
    pub fn terms(&self) -> Vec<String> {
        self.root_ids
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .map(|n| n.term.clone())
            .collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn root_count(&self) -> usize {
        self.root_ids.len()
    }

    /// Total unique members across the tree.
    pub fn total_members(&self) -> u64 {
        let mut all = RoaringBitmap::new();
        for node in self.nodes.values() {
            all |= &node.members;
        }
        all.len()
    }

    /// Snapshot for persistence.
    pub fn snapshot(&self) -> PrimeTreeSnapshot {
        PrimeTreeSnapshot {
            nodes: self.nodes.values().cloned().collect(),
            root_ids: self.root_ids.clone(),
            next_id: self.next_id,
        }
    }

    /// Load from persistence snapshot.
    pub fn load_snapshot(&mut self, snap: PrimeTreeSnapshot) {
        for node in snap.nodes {
            self.nodes.insert(node.id, node);
        }
        self.root_ids = snap.root_ids;
        self.next_id = snap.next_id;
    }

    // --- internals ---

    fn insert_into(&mut self, node_id: u64, memory_id: u32) {
        let Some(node) = self.nodes.get(&node_id) else {
            return;
        };
        if node.children.is_empty() {
            // Leaf node — insert directly
            if let Some(n) = self.nodes.get_mut(&node_id) {
                n.members.insert(memory_id);
            }
        } else {
            // Route to the correct child bucket
            let child_prime = self
                .nodes
                .get(&node.children[0])
                .map(|c| c.prime)
                .unwrap_or(2);
            let bucket = (memory_id as u64) % child_prime;
            let target_child = node
                .children
                .iter()
                .find(|&&cid| {
                    self.nodes
                        .get(&cid)
                        .map_or(false, |c| c.term.ends_with(&format!(":{}", bucket)))
                })
                .copied();
            match target_child {
                Some(cid) => self.insert_into(cid, memory_id),
                None => {
                    // No matching child bucket — add to parent directly
                    if let Some(n) = self.nodes.get_mut(&node_id) {
                        n.members.insert(memory_id);
                    }
                }
            }
        }
    }

    fn maybe_split(&mut self, node_id: u64) {
        let should_split = self.nodes.get(&node_id).map_or(false, |n| {
            n.children.is_empty() && n.members.len() > n.prime
        });
        if should_split {
            self.split(node_id);
        }
    }

    fn find_root_for_term(&self, term: &str) -> Option<u64> {
        self.root_ids
            .iter()
            .copied()
            .find(|&id| self.nodes.get(&id).map_or(false, |n| n.term == term))
    }

    fn collect_descendant_ids(&self, node_id: u64) -> Vec<u64> {
        let Some(node) = self.nodes.get(&node_id) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for &child_id in &node.children {
            result.push(child_id);
            result.extend(self.collect_descendant_ids(child_id));
        }
        result
    }

    fn prune_empty_leaves(&mut self) {
        let empty_leaves: Vec<u64> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.children.is_empty() && n.members.is_empty())
            .map(|(&id, _)| id)
            .collect();

        for leaf_id in empty_leaves {
            self.nodes.remove(&leaf_id);
            self.root_ids.retain(|&id| id != leaf_id);
            // Remove from parent's children list
            for node in self.nodes.values_mut() {
                node.children.retain(|&cid| cid != leaf_id);
            }
        }
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimeTreeSnapshot {
    pub nodes: Vec<PrimeNode>,
    pub root_ids: Vec<u64>,
    pub next_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_creates_root_node() {
        let mut tree = PrimeTree::new();
        tree.insert("weather", 1);
        assert_eq!(tree.root_count(), 1);
        assert_eq!(tree.total_members(), 1);
    }

    #[test]
    fn multiple_terms_create_multiple_roots() {
        let mut tree = PrimeTree::new();
        tree.insert("weather", 1);
        tree.insert("politics", 2);
        tree.insert("weather", 3);
        assert_eq!(tree.root_count(), 2);
        assert_eq!(tree.total_members(), 3);
    }

    #[test]
    fn search_finds_members() {
        let mut tree = PrimeTree::new();
        tree.insert("weather", 1);
        tree.insert("weather", 2);
        tree.insert("politics", 3);

        let result = tree.search_exact("weather");
        assert!(result.contains(1));
        assert!(result.contains(2));
        assert!(!result.contains(3));
    }

    #[test]
    fn split_distributes_members() {
        let mut tree = PrimeTree::new();
        // Prime at depth 0 is 2 — insert 3 members to trigger split
        tree.insert("topic", 1);
        tree.insert("topic", 2);
        tree.insert("topic", 3);

        // After auto-split, root should have children
        let root_id = tree.find_root_for_term("topic").unwrap();
        let root = tree.nodes.get(&root_id).unwrap();
        assert!(!root.children.is_empty() || root.members.len() <= root.prime);

        // All members should still be findable
        let all = tree.collect_members(root_id);
        assert!(all.contains(1));
        assert!(all.contains(2));
        assert!(all.contains(3));
    }

    #[test]
    fn merge_collapses_children() {
        let mut tree = PrimeTree::new();
        for i in 1..=5 {
            tree.insert("topic", i);
        }

        let root_id = tree.find_root_for_term("topic").unwrap();
        let before_nodes = tree.node_count();

        tree.merge(root_id);

        // After merge, should have fewer nodes
        assert!(tree.node_count() <= before_nodes);

        // All members preserved
        let root = tree.nodes.get(&root_id).unwrap();
        assert!(root.children.is_empty());
        assert_eq!(root.members.len(), 5);
    }

    #[test]
    fn remove_member_cleans_up() {
        let mut tree = PrimeTree::new();
        tree.insert("topic", 1);
        tree.insert("topic", 2);
        assert_eq!(tree.total_members(), 2);

        tree.remove_member(1);
        assert_eq!(tree.total_members(), 1);
        assert!(!tree.search_exact("topic").contains(1));
    }

    #[test]
    fn terms_lists_root_terms() {
        let mut tree = PrimeTree::new();
        tree.insert("alpha", 1);
        tree.insert("beta", 2);
        tree.insert("gamma", 3);

        let mut terms = tree.terms();
        terms.sort();
        assert_eq!(terms, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut tree = PrimeTree::new();
        tree.insert("x", 1);
        tree.insert("y", 2);
        tree.insert("x", 3);

        let snap = tree.snapshot();
        let mut tree2 = PrimeTree::new();
        tree2.load_snapshot(snap);

        assert_eq!(tree2.total_members(), 3);
        assert_eq!(tree2.root_count(), tree.root_count());
    }
}
