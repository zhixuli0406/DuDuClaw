//! CJK-safe prompt fingerprinting for JitRL experience retrieval.
//!
//! Mirrors the paper's "Jaccard Similarity Matching: N-gram based similarity
//! for trajectory retrieval" (JitRL, arXiv:2601.18510) with a cheap,
//! deterministic implementation: character-level 3-gram shingles hashed with
//! FNV-1a 64, kept as a bottom-k sketch (the k smallest hashes). Jaccard
//! similarity over two bottom-k sketches approximates the true shingle-set
//! Jaccard — good enough for retrieval ranking, tiny to store.
//!
//! All iteration is over `char`s, never raw bytes, so CJK / emoji input is
//! safe (project convention: no byte slicing).

/// Shingle width in characters.
const SHINGLE_N: usize = 3;

/// Bottom-k sketch size (smallest hashes kept).
const SKETCH_K: usize = 128;

/// FNV-1a 64-bit over a char window.
fn fnv1a_chars(window: &[char]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut buf = [0u8; 4];
    for &c in window {
        for &b in c.encode_utf8(&mut buf).as_bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// Normalize text for fingerprinting: lowercase, keep only alphanumeric and
/// non-ASCII (CJK etc.) characters, drop whitespace/punctuation. Works purely
/// on codepoints — never slices bytes.
fn normalize_chars(text: &str) -> Vec<char> {
    text.chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Compute the bottom-k shingle sketch of a text.
///
/// Returns a sorted, deduplicated vector of at most [`SKETCH_K`] hashes.
/// Texts shorter than the shingle width hash as a single shingle so that
/// very short prompts still fingerprint deterministically.
pub fn shingle_sketch(text: &str) -> Vec<u64> {
    let chars = normalize_chars(text);
    let mut hashes: Vec<u64> = if chars.is_empty() {
        Vec::new()
    } else if chars.len() < SHINGLE_N {
        vec![fnv1a_chars(&chars)]
    } else {
        chars.windows(SHINGLE_N).map(fnv1a_chars).collect()
    };
    hashes.sort_unstable();
    hashes.dedup();
    hashes.truncate(SKETCH_K);
    hashes
}

/// Jaccard similarity of two sorted, deduplicated sketches.
///
/// Returns 0.0 when either sketch is empty. This is an approximation of the
/// true shingle-set Jaccard when the sketches were truncated to bottom-k.
pub fn jaccard(a: &[u64], b: &[u64]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut inter) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

/// Convenience: similarity of two raw texts.
pub fn text_similarity(a: &str, b: &str) -> f32 {
    jaccard(&shingle_sketch(a), &shingle_sketch(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_have_similarity_one() {
        let s = shingle_sketch("please summarize this report for me");
        assert!((jaccard(&s, &s) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn similar_prompts_match_above_threshold() {
        let sim = text_similarity(
            "please summarize this quarterly report for me",
            "please summarize this quarterly report for us",
        );
        assert!(sim > 0.5, "similar prompts should score high, got {sim}");
    }

    #[test]
    fn dissimilar_prompts_do_not_match() {
        let sim = text_similarity(
            "please summarize this quarterly report",
            "write a rust function that reverses a linked list",
        );
        assert!(sim < 0.1, "dissimilar prompts should score low, got {sim}");
    }

    #[test]
    fn cjk_prompts_are_safe_and_discriminative() {
        // Must not panic on multi-byte chars and must still discriminate.
        let similar = text_similarity("請幫我摘要這份季度財務報告", "請幫我摘要這份季度營運報告");
        let different = text_similarity("請幫我摘要這份季度財務報告", "寫一首關於春天的詩");
        assert!(similar > different);
        assert!(similar > 0.4, "got {similar}");
        assert!(different < 0.1, "got {different}");
    }

    #[test]
    fn mixed_cjk_emoji_does_not_panic() {
        let s = shingle_sketch("嗨 👋 hello 世界 🌍 test");
        assert!(!s.is_empty());
    }

    #[test]
    fn short_and_empty_inputs() {
        assert!(shingle_sketch("").is_empty());
        assert_eq!(shingle_sketch("hi").len(), 1);
        assert_eq!(jaccard(&[], &[1, 2]), 0.0);
    }

    #[test]
    fn normalization_ignores_case_and_punctuation() {
        let a = shingle_sketch("Hello, World! How are you?");
        let b = shingle_sketch("hello world how are you");
        assert!((jaccard(&a, &b) - 1.0).abs() < f32::EPSILON);
    }
}
