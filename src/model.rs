use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub id: u32,
    pub tags: std::collections::BTreeMap<String, String>,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Cosine,
    L2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub id: u32,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub ids: Vec<u32>,
}
