//! `ferricula-episode`: Structured, versioned episode prototype establishing
//! observation/interpretation separation, link projection, evidential transitions,
//! and bounded query semantics using `OverlayLog` on channel `episode_v1`.

pub mod overlay;
pub use overlay as memory_overlay;

pub mod model;
pub mod causal;
pub mod projection;
pub mod adapter;
pub mod query;

pub use adapter::{EpisodeAdapter, DEFAULT_EPISODES_FILENAME};
pub use model::*;
pub use projection::{EpisodeProjection, StoredGoal, StoredHypothesis, StoredObservation};
pub use query::{
    EpisodeBundle, EpisodeQueryRequest, EpisodeQueryResponse, EvidentialDeltaView,
    InterpretationView, LinkedContextItem, ObservationView, RetrievalMode, query_episodes,
};
