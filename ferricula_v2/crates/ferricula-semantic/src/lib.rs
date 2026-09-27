pub mod store {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Chunk {
        pub id: String,
        pub text: String,
        /// Organize embedding (768d, gtr-t5-base)
        pub embedding: Vec<f32>,
        /// Retrieve embedding (1536d, ada-002) — optional
        #[serde(default)]
        pub embedding_retrieve: Option<Vec<f32>>,
        pub token_count: usize,
        pub source: Option<String>,
        pub metadata: serde_json::Value,
        pub created_at: DateTime<Utc>,
        /// Vedanā — feeling tone at time of memory creation (Abhidharma)
        #[serde(default)]
        pub emotion_primary: Option<String>,
        #[serde(default)]
        pub emotion_secondary: Option<String>,
        /// Whether embeddings are encrypted with agent identity key
        #[serde(default)]
        pub encrypted: bool,
        /// Agent that owns this chunk (for encryption key lookup)
        #[serde(default)]
        pub agent_id: Option<String>,
    }
}

#[cfg(feature = "ml")]
pub mod chunker;
pub mod crypto;
#[cfg(feature = "ml")]
pub mod embedder;
#[cfg(feature = "ml")]
pub mod inverter;
pub mod openai;
pub mod similarity;
