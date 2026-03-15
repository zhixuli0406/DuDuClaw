use duduclaw_core::error::{DuDuClawError, Result};

/// Compute cosine similarity between two vectors.
///
/// Returns `0.0` when vectors differ in length, are empty, or either
/// has zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// In-memory vector search index for memory entries.
///
/// Stores `(memory_id, embedding)` pairs and supports nearest-neighbour
/// queries via cosine similarity.
pub struct VectorIndex {
    vectors: Vec<(String, Vec<f32>)>,
    dimension: usize,
}

impl VectorIndex {
    /// Create a new index that expects embeddings of the given `dimension`.
    pub fn new(dimension: usize) -> Self {
        Self {
            vectors: Vec::new(),
            dimension,
        }
    }

    /// Insert an embedding for the given memory id.
    ///
    /// Returns an error when `embedding.len() != self.dimension`.
    pub fn add(&mut self, id: &str, embedding: Vec<f32>) -> Result<()> {
        if embedding.len() != self.dimension {
            return Err(DuDuClawError::Memory(format!(
                "Expected dimension {}, got {}",
                self.dimension,
                embedding.len()
            )));
        }
        self.vectors.push((id.to_string(), embedding));
        Ok(())
    }

    /// Remove all embeddings for the given memory id.
    pub fn remove(&mut self, id: &str) {
        self.vectors.retain(|(vid, _)| vid != id);
    }

    /// Return the top-k most similar memory ids for the given query vector.
    ///
    /// Results are sorted by descending similarity score.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension {
            return vec![];
        }

        let mut scored: Vec<(String, f32)> = self
            .vectors
            .iter()
            .map(|(id, vec)| (id.clone(), cosine_similarity(query, vec)))
            .collect();

        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored
    }

    /// Number of stored embeddings.
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_similarity_mismatched_len() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn test_vector_index_search() {
        let mut index = VectorIndex::new(3);
        index.add("a", vec![1.0, 0.0, 0.0]).unwrap();
        index.add("b", vec![0.0, 1.0, 0.0]).unwrap();
        index.add("c", vec![0.9, 0.1, 0.0]).unwrap();

        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "a"); // exact match first
        assert_eq!(results[1].0, "c"); // close second
    }

    #[test]
    fn test_vector_index_wrong_dimension() {
        let mut index = VectorIndex::new(3);
        let result = index.add("x", vec![1.0, 2.0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_vector_index_remove() {
        let mut index = VectorIndex::new(2);
        index.add("a", vec![1.0, 0.0]).unwrap();
        index.add("b", vec![0.0, 1.0]).unwrap();
        assert_eq!(index.len(), 2);

        index.remove("a");
        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_vector_index_search_wrong_query_dim() {
        let index = VectorIndex::new(3);
        let results = index.search(&[1.0, 2.0], 5);
        assert!(results.is_empty());
    }
}
