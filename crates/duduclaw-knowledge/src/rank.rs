//! Reciprocal rank fusion of the ranked lists (FTS, short-term `LIKE`, and
//! vectors in a later release): `score = Σ 1 / (K + rank)`, rank 1-based.
//! Ties break on the smaller chunk id, so the order is deterministic. At most
//! [`PER_SOURCE_CAP`] chunks of one source make the final list.

use std::collections::HashMap;

pub const K: f32 = 60.0;
pub const PER_SOURCE_CAP: usize = 3;

/// A fused candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Fused {
    pub chunk_id: i64,
    pub score: f32,
    /// 1-based rank per input list (`None` = not in that list).
    pub ranks: Vec<Option<u32>>,
}

/// Fuse `lists` (each best-first).
pub fn rrf(lists: &[Vec<i64>]) -> Vec<Fused> {
    let mut by_id: HashMap<i64, Fused> = HashMap::new();
    for (li, list) in lists.iter().enumerate() {
        for (pos, &id) in list.iter().enumerate() {
            let rank = pos as u32 + 1;
            let entry = by_id.entry(id).or_insert_with(|| Fused {
                chunk_id: id,
                score: 0.0,
                ranks: vec![None; lists.len()],
            });
            if entry.ranks[li].is_none() {
                entry.ranks[li] = Some(rank);
                entry.score += 1.0 / (K + rank as f32);
            }
        }
    }
    let mut out: Vec<Fused> = by_id.into_values().collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk_id.cmp(&b.chunk_id)));
    out
}

/// Keep at most [`PER_SOURCE_CAP`] per source, then the first `limit`.
pub fn cap_per_source(fused: Vec<Fused>, source_of: &HashMap<i64, String>, limit: usize) -> Vec<Fused> {
    let mut per: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    for f in fused {
        let Some(src) = source_of.get(&f.chunk_id) else {
            continue;
        };
        let n = per.entry(src.as_str()).or_default();
        if *n >= PER_SOURCE_CAP {
            continue;
        }
        *n += 1;
        out.push(f);
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_rewards_agreement_and_is_deterministic() {
        let fused = rrf(&[vec![10, 20, 30], vec![30, 40]]);
        assert_eq!(fused[0].chunk_id, 30);
        assert_eq!(fused[0].ranks, vec![Some(3), Some(1)]);
        // 20 (rank 2 in list 0) and 40 (rank 2 in list 1) tie: smaller id first.
        let order: Vec<i64> = fused.iter().map(|f| f.chunk_id).collect();
        assert_eq!(order, vec![30, 10, 20, 40]);
        assert_eq!(rrf(&[vec![10, 20, 30], vec![30, 40]]), fused);
    }

    #[test]
    fn per_source_cap_applies() {
        let fused = rrf(&[vec![1, 2, 3, 4, 5]]);
        let src: HashMap<i64, String> = [(1, "a"), (2, "a"), (3, "a"), (4, "a"), (5, "b")]
            .into_iter()
            .map(|(i, s)| (i, s.to_string()))
            .collect();
        let kept: Vec<i64> = cap_per_source(fused, &src, 10).iter().map(|f| f.chunk_id).collect();
        assert_eq!(kept, vec![1, 2, 3, 5]);
    }
}
