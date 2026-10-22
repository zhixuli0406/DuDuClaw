//! Pooling and vector math. Pure functions over flat `[batch, seq, hidden]`
//! buffers so they can be tested on tiny fixtures without a model.

/// Mask-weighted mean over the sequence axis (e5 `average_pool`): padded
/// positions contribute nothing; a row with no real token yields zeros.
pub fn mean_pool(
    hidden: &[f32],
    mask: &[i64],
    batch: usize,
    seq: usize,
    dim: usize,
) -> Vec<Vec<f32>> {
    (0..batch)
        .map(|b| {
            let mut acc = vec![0f32; dim];
            let mut n = 0f32;
            for t in 0..seq {
                if mask[b * seq + t] == 0 {
                    continue;
                }
                n += 1.0;
                let row = &hidden[(b * seq + t) * dim..(b * seq + t + 1) * dim];
                for (a, v) in acc.iter_mut().zip(row) {
                    *a += v;
                }
            }
            if n > 0.0 {
                for a in &mut acc {
                    *a /= n;
                }
            }
            acc
        })
        .collect()
}

/// The hidden state of the last real (mask = 1) token of each row. Works
/// for left and right padding alike; a row with no real token yields zeros.
pub fn last_token_pool(
    hidden: &[f32],
    mask: &[i64],
    batch: usize,
    seq: usize,
    dim: usize,
) -> Vec<Vec<f32>> {
    (0..batch)
        .map(|b| match (0..seq).rev().find(|&t| mask[b * seq + t] != 0) {
            Some(t) => hidden[(b * seq + t) * dim..(b * seq + t + 1) * dim].to_vec(),
            None => vec![0f32; dim],
        })
        .collect()
}

/// Keep the first `dim` components (Matryoshka truncation) and L2
/// normalise.
pub fn truncate_and_normalize(mut v: Vec<f32>, dim: usize) -> Vec<f32> {
    v.truncate(dim);
    l2_normalize(v)
}

/// Scale to unit length (a zero vector stays zero).
pub fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Dot product; equals cosine similarity for unit vectors.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Little-endian `f32` bytes, the `embeddings.vec` BLOB format.
pub fn encode(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Inverse of [`encode`]; `None` when the length is not `dim * 4`.
pub fn decode(bytes: &[u8], dim: usize) -> Option<Vec<f32>> {
    if bytes.len() != dim * 4 {
        return None;
    }
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // batch 2, seq 3, dim 2.
    const H: [f32; 12] = [
        1.0, 2.0, 3.0, 4.0, 100.0, 100.0, // row 0: two real tokens, one pad
        5.0, 6.0, 7.0, 8.0, 9.0, 10.0, // row 1: three real tokens
    ];

    #[test]
    fn mean_pool_ignores_padding() {
        let mask = [1, 1, 0, 1, 1, 1];
        let out = mean_pool(&H, &mask, 2, 3, 2);
        assert_eq!(out[0], vec![2.0, 3.0]);
        assert_eq!(out[1], vec![7.0, 8.0]);
        let empty = mean_pool(&H, &[0, 0, 0, 0, 0, 0], 2, 3, 2);
        assert_eq!(empty[0], vec![0.0, 0.0]);
    }

    #[test]
    fn last_token_pool_handles_right_and_left_padding() {
        // Right padding: last real token of row 0 is t=1.
        let out = last_token_pool(&H, &[1, 1, 0, 1, 1, 1], 2, 3, 2);
        assert_eq!(out[0], vec![3.0, 4.0]);
        assert_eq!(out[1], vec![9.0, 10.0]);
        // Left padding: row 0 real tokens at t=1,2.
        let out = last_token_pool(&H, &[0, 1, 1, 1, 1, 1], 2, 3, 2);
        assert_eq!(out[0], vec![100.0, 100.0]);
    }

    #[test]
    fn normalisation_and_truncation() {
        let v = l2_normalize(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert!((dot(&v, &v) - 1.0).abs() < 1e-6);
        let t = truncate_and_normalize(vec![3.0, 4.0, 12.0], 2);
        assert_eq!(t.len(), 2);
        assert!((dot(&t, &t) - 1.0).abs() < 1e-6);
        assert_eq!(l2_normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn blob_round_trip() {
        let v = vec![0.25f32, -1.5, 3.0];
        let b = encode(&v);
        assert_eq!(b.len(), 12);
        assert_eq!(decode(&b, 3).unwrap(), v);
        assert!(decode(&b, 4).is_none());
    }
}
