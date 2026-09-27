pub mod engine;
pub mod graph;
pub mod memory;
pub mod model;
pub mod persist;
pub mod prime_tree;
pub mod skg;
pub mod sparse;
pub mod transform;

pub use engine::Engine;
pub use model::{DistanceMetric, MemoryRef, QueryResult, Row, VectorHit};
pub use persist::DurableEngine;
pub use graph::{EdgeKind, MemoryGraph};
pub use memory::{LifecycleState, MemoryRecord, MemoryStore, ResonanceGate};
pub use prime_tree::PrimeTree;
pub use skg::{SkgEdge, SkgSnapshot, SkgState, SkgUpdateSummary};
