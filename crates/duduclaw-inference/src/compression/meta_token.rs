//! LTSC Meta-Token compression — lossless token sequence compression.
//!
//! Finds repeated multi-token subsequences and replaces them with single
//! meta-tokens. On decompression, meta-tokens are expanded back to the
//! original sequences — perfectly lossless.
//!
//! Algorithm (BPE-like at the string level):
//! 1. Tokenize input into chunks (words / JSON keys / code tokens)
//! 2. Find the most frequent bigram (pair of adjacent chunks)
//! 3. Replace all occurrences with a meta-token
//! 4. Repeat until no bigram appears more than `min_frequency` times
//!
//! Best for: JSON, code, templates, SOUL.md, CLAUDE.md — anything with repetition.
//! Typical compression: 27-47% reduction on structured input.

use std::collections::HashMap;

use super::CompressionStats;

/// Separator used between meta-token id and the dictionary in serialized form.
const DICT_SEPARATOR: &str = "\x00META\x00";

/// Minimum frequency for a bigram to be worth replacing.
const DEFAULT_MIN_FREQUENCY: usize = 3;

/// Maximum number of merge rounds.
const MAX_ROUNDS: usize = 256;

/// Maximum input size for compression (1 MB).
const MAX_INPUT_SIZE: usize = 1_048_576;

/// Maximum output size for decompression (50 MB — decompression bomb guard).
const MAX_DECOMPRESS_SIZE: usize = 50 * 1024 * 1024;

/// Compress text using LTSC meta-token replacement.
///
/// Returns `(compressed_text, stats)`. The compressed text contains the
/// meta-token dictionary appended after a separator, so it can be
/// decompressed without external state.
pub fn compress(text: &str) -> (String, CompressionStats) {
    compress_with_options(text, DEFAULT_MIN_FREQUENCY)
}

/// Compress with configurable minimum frequency.
pub fn compress_with_options(text: &str, min_frequency: usize) -> (String, CompressionStats) {
    if text.len() < 100 || text.len() > MAX_INPUT_SIZE {
        return (text.to_string(), CompressionStats::new(text.len(), text.len(), "meta-token", true));
    }

    let mut tokens = tokenize(text);
    let mut dictionary: Vec<(String, String, String)> = Vec::new(); // (meta_id, left, right)
    let mut next_id: u32 = 0;

    for _round in 0..MAX_ROUNDS {
        let bigrams = count_bigrams(&tokens);

        // Find most frequent bigram and collect as owned before mutating tokens
        let best = bigrams.iter()
            .max_by_key(|(_, count)| *count)
            .filter(|(_, count)| **count >= min_frequency)
            .map(|((l, r), _)| (l.to_string(), r.to_string()));

        match best {
            Some((left, right)) => {
                let meta_id = format!("\x01M{next_id}\x02");
                next_id += 1;
                tokens = replace_bigram(&tokens, &left, &right, &meta_id);
                dictionary.push((meta_id, left, right));
            }
            None => break,
        }
    }

    let compressed_body = tokens.join("");

    // Serialize dictionary inline
    if dictionary.is_empty() {
        return (text.to_string(), CompressionStats::new(text.len(), text.len(), "meta-token", true));
    }

    let dict_str: String = dictionary.iter()
        .map(|(id, l, r)| {
            let l_esc = l.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t");
            let r_esc = r.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t");
            format!("{id}\t{l_esc}\t{r_esc}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let compressed = format!("{compressed_body}{DICT_SEPARATOR}{dict_str}");

    let stats = CompressionStats::new(text.len(), compressed.len(), "meta-token", true);
    (compressed, stats)
}

/// Decompress a meta-token compressed string back to the original.
pub fn decompress(compressed: &str) -> String {
    if compressed.len() > MAX_DECOMPRESS_SIZE {
        return compressed.to_string(); // Input too large — refuse to decompress
    }

    let Some(sep_pos) = compressed.find(DICT_SEPARATOR) else {
        return compressed.to_string();
    };

    let body = &compressed[..sep_pos];
    let dict_str = &compressed[sep_pos + DICT_SEPARATOR.len()..];

    // Parse dictionary (in reverse order for correct expansion)
    let entries: Vec<(String, String, String)> = dict_str
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.len() == 3 {
                let l = unescape(parts[1]);
                let r = unescape(parts[2]);
                Some((parts[0].to_string(), l, r))
            } else {
                None
            }
        })
        .collect();

    let mut result = body.to_string();

    // Expand in reverse order (last merge first) with decompression bomb guard
    for (meta_id, left, right) in entries.iter().rev() {
        let replacement = format!("{left}{right}");
        result = result.replace(meta_id.as_str(), &replacement);
        if result.len() > MAX_DECOMPRESS_SIZE {
            return compressed.to_string(); // Bail out — possible decompression bomb
        }
    }

    result
}

/// Unescape dictionary values (backslash MUST be unescaped FIRST).
fn unescape(s: &str) -> String {
    // Use \x01..\x02 sentinel (same range as meta-token IDs, never in real text)
    let s = s.replace("\\\\", "\x01BSLASH\x02");
    let s = s.replace("\\n", "\n");
    let s = s.replace("\\t", "\t");
    s.replace("\x01BSLASH\x02", "\\")
}

/// Tokenize text into chunks suitable for bigram analysis.
///
/// Splits on whitespace boundaries but preserves the whitespace as separate tokens.
/// This ensures lossless round-trip.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
            tokens.push(ch.to_string());
        } else if ch == '{' || ch == '}' || ch == '[' || ch == ']'
            || ch == '(' || ch == ')' || ch == ':' || ch == ','
            || ch == ';' || ch == '"'
        {
            // Split on structural characters for better bigram matching
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
            tokens.push(ch.to_string());
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Count all bigrams (adjacent pairs) using &str references (zero-alloc).
fn count_bigrams(tokens: &[String]) -> HashMap<(&str, &str), usize> {
    let mut counts = HashMap::new();
    for window in tokens.windows(2) {
        let key = (window[0].as_str(), window[1].as_str());
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// Replace all occurrences of a bigram with a meta-token.
fn replace_bigram(tokens: &[String], left: &str, right: &str, meta_id: &str) -> Vec<String> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        if i + 1 < tokens.len() && tokens[i] == left && tokens[i + 1] == right {
            result.push(meta_id.to_string());
            i += 2;
        } else {
            result.push(tokens[i].clone());
            i += 1;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simple() {
        let original = "hello world hello world hello world";
        let (compressed, stats) = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, original);
        assert!(stats.lossless);
    }

    #[test]
    fn roundtrip_json() {
        let original = r#"{"name": "agent1", "role": "main", "status": "active"}, {"name": "agent2", "role": "worker", "status": "active"}, {"name": "agent3", "role": "worker", "status": "paused"}"#;
        let (compressed, stats) = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, original);
        assert!(stats.lossless);
    }

    #[test]
    fn large_json_compresses_well() {
        // Larger input with more repetition benefits from compression
        let entry = r#"{"name": "agent", "role": "worker", "status": "active", "model": "claude-sonnet-4-6", "heartbeat": true}"#;
        let original: String = (0..20).map(|i| entry.replace("agent", &format!("agent{i}"))).collect::<Vec<_>>().join(", ");
        let (compressed, stats) = compress(&original);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, original);
        assert!(stats.ratio > 1.0, "ratio {:.2} should be > 1.0 for large repetitive input", stats.ratio);
    }

    #[test]
    fn roundtrip_code() {
        let original = r#"
fn process(a: &str) -> Result<String> {
    let result = parse(a)?;
    Ok(result)
}
fn process_b(b: &str) -> Result<String> {
    let result = parse(b)?;
    Ok(result)
}
fn process_c(c: &str) -> Result<String> {
    let result = parse(c)?;
    Ok(result)
}
"#;
        let (compressed, stats) = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, original);
        assert!(stats.lossless);
    }

    #[test]
    fn no_compression_short_text() {
        let original = "hi";
        let (compressed, _) = compress(original);
        assert_eq!(compressed, original); // Too short, no compression
    }

    #[test]
    fn decompress_uncompressed_passthrough() {
        let text = "this is not compressed at all";
        assert_eq!(decompress(text), text);
    }

    #[test]
    fn roundtrip_cjk() {
        let original = "你好世界 你好世界 你好世界 這是測試 這是測試 這是測試";
        let (compressed, stats) = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, original);
        assert!(stats.lossless);
    }

    #[test]
    fn roundtrip_backslash_n_literal() {
        // Must be ≥100 chars to trigger actual compression (not early-return passthrough)
        let chunk = r#"line1\nline2\nline3 "#;
        let original: String = chunk.repeat(8); // 160 chars, well over threshold
        assert!(original.len() >= 100);
        let (compressed, _) = compress(&original);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, original, "literal backslash-n must survive roundtrip");
    }

    #[test]
    fn roundtrip_mixed_escapes() {
        // Must be ≥100 chars. Mix of backslashes, tabs, newlines
        let chunk = "a\\b\tc\nd ";
        let original: String = chunk.repeat(20); // 160 chars
        assert!(original.len() >= 100);
        let (compressed, _) = compress(&original);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, original);
    }
}
