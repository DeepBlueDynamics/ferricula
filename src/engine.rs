use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use roaring::RoaringBitmap;

use crate::model::{DistanceMetric, QueryResult, Row, VectorHit};

#[derive(Debug, Default)]
pub struct Engine {
    rows: BTreeMap<u32, Row>,
    all_ids: RoaringBitmap,
    // field -> value -> bitmap(ids)
    tag_index: HashMap<String, HashMap<String, RoaringBitmap>>,
    vector_dim: Option<usize>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&mut self, row: Row) -> Result<()> {
        self.validate_vector_dim(row.vector.len())?;
        if let Some(previous) = self.rows.remove(&row.id) {
            self.remove_from_indexes(&previous);
        }

        self.insert_into_indexes(&row);
        self.all_ids.insert(row.id);
        self.rows.insert(row.id, row);
        Ok(())
    }

    pub fn delete(&mut self, id: u32) -> bool {
        let Some(previous) = self.rows.remove(&id) else {
            return false;
        };
        self.remove_from_indexes(&previous);
        self.all_ids.remove(id);
        true
    }

    pub fn get(&self, id: u32) -> Option<&Row> {
        self.rows.get(&id)
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn all_bitmap(&self) -> RoaringBitmap {
        self.all_ids.clone()
    }

    pub fn bitmap_for_tag_eq(&self, field: &str, value: &str) -> RoaringBitmap {
        self.tag_index
            .get(&field.to_lowercase())
            .and_then(|values| values.get(value))
            .cloned()
            .unwrap_or_default()
    }

    /// Returns a bitmap of all IDs where `field` parses as `u32` and the parsed
    /// value satisfies the requested numeric range. Used for reference fields
    /// like `page`, `chapter`, `volume` that are stored in `tag_index` as their
    /// decimal string representation.
    ///
    /// `low`/`high` of `None` means unbounded on that side. `inclusive_low` /
    /// `inclusive_high` choose between `<`/`<=` and `>`/`>=` semantics.
    /// Tag values that fail to parse as `u32` are silently skipped — this lets
    /// the same field accept both numeric and string values without poisoning
    /// range queries.
    pub fn bitmap_for_tag_range(
        &self,
        field: &str,
        low: Option<u32>,
        high: Option<u32>,
        inclusive_low: bool,
        inclusive_high: bool,
    ) -> RoaringBitmap {
        let Some(values) = self.tag_index.get(&field.to_lowercase()) else {
            return RoaringBitmap::new();
        };
        let mut result = RoaringBitmap::new();
        for (val_str, bitmap) in values {
            let Ok(n) = val_str.parse::<u32>() else {
                continue;
            };
            let lo_ok = match low {
                None => true,
                Some(lo) => {
                    if inclusive_low {
                        n >= lo
                    } else {
                        n > lo
                    }
                }
            };
            let hi_ok = match high {
                None => true,
                Some(hi) => {
                    if inclusive_high {
                        n <= hi
                    } else {
                        n < hi
                    }
                }
            };
            if lo_ok && hi_ok {
                result |= bitmap;
            }
        }
        result
    }

    /// Returns all distinct tag values for `field`, sorted.  Uses the
    /// already-maintained `tag_index` — O(k) where k is the number of
    /// distinct values for that field.
    pub fn distinct_tag_values(&self, field: &str) -> Vec<String> {
        let Some(values) = self.tag_index.get(&field.to_lowercase()) else {
            return Vec::new();
        };
        let mut v: Vec<String> = values.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn bitmap_jaccard(&self, left: &RoaringBitmap, right: &RoaringBitmap) -> f64 {
        let intersection = left.intersection_len(right);
        let union = left.union_len(right);
        if union == 0 {
            return 1.0;
        }
        intersection as f64 / union as f64
    }

    pub fn vector_topk(
        &self,
        query: &[f32],
        k: usize,
        metric: DistanceMetric,
        candidate_filter: Option<&RoaringBitmap>,
    ) -> Vec<VectorHit> {
        let mut hits = Vec::new();
        for row in self.rows.values() {
            if row.vector.len() != query.len() {
                continue;
            }
            if let Some(filter) = candidate_filter {
                if !filter.contains(row.id) {
                    continue;
                }
            }
            let score = match metric {
                DistanceMetric::Cosine => cosine_similarity(query, &row.vector),
                DistanceMetric::L2 => -l2_distance(query, &row.vector),
            };
            hits.push(VectorHit { id: row.id, score });
        }

        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        hits
    }

    pub fn vector_topk_bitmap(
        &self,
        query: &[f32],
        k: usize,
        metric: DistanceMetric,
        candidate_filter: Option<&RoaringBitmap>,
    ) -> RoaringBitmap {
        let mut bitmap = RoaringBitmap::new();
        for hit in self.vector_topk(query, k, metric, candidate_filter) {
            bitmap.insert(hit.id);
        }
        bitmap
    }

    pub fn execute_sql(&self, sql: &str) -> Result<QueryResult> {
        crate::sql::execute_sql(self, sql)
    }

    pub fn execute_sql_with_embed(&self, sql: &str, shivvr_url: Option<&str>) -> Result<QueryResult> {
        crate::sql::execute_sql_with_embed(self, None, sql, shivvr_url)
    }

    pub fn rows_iter(&self) -> impl Iterator<Item = &Row> {
        self.rows.values()
    }

    fn validate_vector_dim(&mut self, dim: usize) -> Result<()> {
        if let Some(current_dim) = self.vector_dim {
            if current_dim != dim {
                bail!(
                    "vector dimension mismatch: expected {}, got {}",
                    current_dim,
                    dim
                );
            }
        } else {
            self.vector_dim = Some(dim);
        }
        Ok(())
    }

    fn insert_into_indexes(&mut self, row: &Row) {
        for (field, value) in &row.tags {
            self.tag_index
                .entry(field.to_lowercase())
                .or_default()
                .entry(value.clone())
                .or_default()
                .insert(row.id);
        }
    }

    fn remove_from_indexes(&mut self, row: &Row) {
        for (field, value) in &row.tags {
            if let Some(values) = self.tag_index.get_mut(&field.to_lowercase()) {
                if let Some(bitmap) = values.get_mut(value) {
                    bitmap.remove(row.id);
                    if bitmap.is_empty() {
                        values.remove(value);
                    }
                }
                if values.is_empty() {
                    self.tag_index.remove(&field.to_lowercase());
                }
            }
        }
    }
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let mut dot = 0.0_f32;
    let mut norm_left = 0.0_f32;
    let mut norm_right = 0.0_f32;

    for (a, b) in left.iter().zip(right.iter()) {
        dot += a * b;
        norm_left += a * a;
        norm_right += b * b;
    }

    if norm_left <= f32::EPSILON || norm_right <= f32::EPSILON {
        return 0.0;
    }
    dot / (norm_left.sqrt() * norm_right.sqrt())
}

fn l2_distance(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right.iter())
        .map(|(a, b)| {
            let d = a - b;
            d * d
        })
        .sum::<f32>()
        .sqrt()
}


#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn row(id: u32, region: &str, tier: &str, vector: Vec<f32>) -> Row {
        let mut tags = BTreeMap::new();
        tags.insert("region".to_string(), region.to_string());
        tags.insert("tier".to_string(), tier.to_string());
        Row {
            id,
            tags,
            vector,
            refs: None,
        }
    }

    #[test]
    fn bitmap_jaccard_works() {
        let mut engine = Engine::new();
        engine
            .upsert(row(1, "us", "gold", vec![1.0, 0.0, 0.0]))
            .unwrap();
        engine
            .upsert(row(2, "us", "silver", vec![0.0, 1.0, 0.0]))
            .unwrap();
        engine
            .upsert(row(3, "eu", "gold", vec![0.0, 0.0, 1.0]))
            .unwrap();

        let us = engine.bitmap_for_tag_eq("region", "us");
        let gold = engine.bitmap_for_tag_eq("tier", "gold");
        let j = engine.bitmap_jaccard(&us, &gold);
        assert!((j - (1.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn cosine_topk_works() {
        let mut engine = Engine::new();
        engine
            .upsert(row(1, "us", "gold", vec![1.0, 0.0, 0.0]))
            .unwrap();
        engine
            .upsert(row(2, "us", "silver", vec![0.8, 0.2, 0.0]))
            .unwrap();
        engine
            .upsert(row(3, "eu", "gold", vec![0.0, 1.0, 0.0]))
            .unwrap();

        let hits = engine.vector_topk(&[1.0, 0.0, 0.0], 2, DistanceMetric::Cosine, None);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, 1);
        assert_eq!(hits[1].id, 2);
    }
}
