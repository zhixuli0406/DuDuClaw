use duduclaw_core::types::MemoryEntry;

/// Rank search results by simple keyword-match count against `query`.
///
/// Results with more matching terms appear first.
pub fn rank_results(results: Vec<MemoryEntry>, query: &str) -> Vec<MemoryEntry> {
    let query_terms: Vec<String> = query.split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();

    let mut scored: Vec<(f64, MemoryEntry)> = results
        .into_iter()
        .map(|entry| {
            let content_lower = entry.content.to_lowercase();
            let score = query_terms
                .iter()
                .filter(|term| content_lower.contains(term.as_str()))
                .count() as f64;
            (score, entry)
        })
        .collect();

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// Filter memories that contain at least one of the given `tags`.
///
/// Returns all entries unchanged when `tags` is empty.
pub fn filter_by_tags(entries: &[MemoryEntry], tags: &[String]) -> Vec<MemoryEntry> {
    if tags.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| tags.iter().any(|t| e.tags.contains(t)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(content: &str, tags: Vec<&str>) -> MemoryEntry {
        MemoryEntry {
            id: String::new(),
            agent_id: String::new(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: tags.into_iter().map(String::from).collect(),
            embedding: None,
        }
    }

    #[test]
    fn rank_orders_by_match_count() {
        let results = vec![
            entry("apple pie", vec![]),
            entry("apple banana cherry", vec![]),
            entry("banana only", vec![]),
        ];
        let ranked = rank_results(results, "apple cherry");
        // "apple banana cherry" matches both terms (score 2) → first
        assert!(ranked[0].content.contains("apple") && ranked[0].content.contains("cherry"));
        // "apple pie" matches one term (score 1) → second
        assert_eq!(ranked[1].content, "apple pie");
        // "banana only" matches zero terms (score 0) → last
        assert_eq!(ranked[2].content, "banana only");
    }

    #[test]
    fn filter_by_tags_empty_returns_all() {
        let entries = vec![entry("a", vec!["x"]), entry("b", vec!["y"])];
        let filtered = filter_by_tags(&entries, &[]);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_tags_selects_matching() {
        let entries = vec![
            entry("a", vec!["important"]),
            entry("b", vec!["trivial"]),
            entry("c", vec!["important", "urgent"]),
        ];
        let filtered = filter_by_tags(&entries, &["important".to_string()]);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|e| e.tags.contains(&"important".to_string())));
    }
}
