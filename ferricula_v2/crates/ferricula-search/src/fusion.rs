use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Tag {
    pub surface: String,
    pub output: String,
}

/// Fuses two ranked lists of items (lexical and semantic) using Reciprocal Rank Fusion (RRF),
/// and optionally applies FST dynamic intent boosting based on tag matches in chunk text.
pub fn reciprocal_rank_fusion(
    lexical_ranking: &[String],
    semantic_ranking: &[String],
    chunk_texts: &HashMap<String, String>,
    tags: &[Tag],
    k: f32, // RRF constant, e.g. 60.0
) -> Vec<(String, f32)> {
    let mut vector_ranks: HashMap<String, usize> = HashMap::new();
    for (rank, id) in semantic_ranking.iter().enumerate() {
        vector_ranks.insert(id.clone(), rank);
    }

    let mut lexical_ranks: HashMap<String, usize> = HashMap::new();
    for (rank, id) in lexical_ranking.iter().enumerate() {
        lexical_ranks.insert(id.clone(), rank);
    }

    // Gather all unique chunk IDs from the chunk_texts keys
    let mut all_ids: Vec<String> = chunk_texts.keys().cloned().collect();
    // Also include any IDs from the rankings that might not be in chunk_texts
    for id in lexical_ranking.iter().chain(semantic_ranking.iter()) {
        if !chunk_texts.contains_key(id) && !all_ids.contains(id) {
            all_ids.push(id.clone());
        }
    }

    let mut final_scores: Vec<(String, f32)> = all_ids
        .into_iter()
        .map(|id| {
            let rrf_sem = vector_ranks.get(&id)
                .map(|&rank| 1.0 / (k + rank as f32))
                .unwrap_or(0.0);
            let rrf_lex = lexical_ranks.get(&id)
                .map(|&rank| 1.0 / (k + rank as f32))
                .unwrap_or(0.0);
            let mut combined_score = rrf_sem + rrf_lex;

            // FST dynamic intent boosting
            if let Some(text) = chunk_texts.get(&id) {
                let normalized_text = text.to_lowercase();
                for tag in tags {
                    let surface_matched = normalized_text.contains(&tag.surface.to_lowercase());
                    let output_matched = normalized_text.contains(&tag.output.to_lowercase());
                    if surface_matched || output_matched {
                        combined_score += 0.05f32; // Boost rank dynamically in the RRF domain
                    }
                }
            }

            (id, combined_score)
        })
        .collect();

    final_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    final_scores
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_fusion_and_boosting() {
        let lexical = vec!["doc1".to_string(), "doc2".to_string(), "doc3".to_string()];
        let semantic = vec!["doc3".to_string(), "doc1".to_string(), "doc4".to_string()];
        
        let mut texts = HashMap::new();
        texts.insert("doc1".to_string(), "This is document number one about rust coding.".to_string());
        texts.insert("doc2".to_string(), "Here is a document about python language.".to_string());
        texts.insert("doc3".to_string(), "And here is a document talking about neural networks and artificial intelligence.".to_string());
        texts.insert("doc4".to_string(), "Nothing interesting here.".to_string());

        let tags = vec![
            Tag { surface: "neural".to_string(), output: "AI".to_string() }
        ];

        let fused = reciprocal_rank_fusion(&lexical, &semantic, &texts, &tags, 60.0);

        // Expected scores calculation with k = 60.0:
        // doc1: rank 0 in lex (1/60), rank 1 in sem (1/61). Combined = 1/60 + 1/61 = 0.016666 + 0.016393 = 0.03306. No tag matches "neural" or "AI" -> 0.03306.
        // doc2: rank 1 in lex (1/61), no sem (0.0). Combined = 0.016393. No tag match -> 0.01639.
        // doc3: rank 2 in lex (1/62), rank 0 in sem (1/60). Combined = 1/62 + 1/60 = 0.016129 + 0.016666 = 0.03279. Contains "neural" -> boost +0.05 -> 0.08279.
        // doc4: no lex (0.0), rank 2 in sem (1/62 = 0.016129). No tag match -> 0.01613.

        // So with boosting, doc3 should rank 1st, then doc1, then doc2, then doc4.
        assert_eq!(fused[0].0, "doc3");
        assert_eq!(fused[1].0, "doc1");
        assert_eq!(fused[2].0, "doc2");
        assert_eq!(fused[3].0, "doc4");

        // Assert score thresholds
        assert!(fused[0].1 > 0.08);
        assert!((fused[1].1 - 0.033).abs() < 1e-3);
    }
}
