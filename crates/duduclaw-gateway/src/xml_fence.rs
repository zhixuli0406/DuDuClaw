//! Case-insensitive XML closing-tag fencing for untrusted text embedded in
//! LLM prompts.
//!
//! Lifted out of the removed `gvu::generator` (S11, 2026-09-29): the legacy
//! SOUL.md rewrite path is gone, but this helper is a general
//! prompt-injection defence with live callers outside evolution
//! (`wiki_ingest`'s conversation-distillation prompts), so it keeps a neutral
//! home instead of disappearing with its former module.

/// Case-insensitive XML closing tag escape to prevent injection.
///
/// Uses a byte-offset mapping between `content` and its `to_lowercase()` form
/// to handle Unicode chars whose lowercase representation has different byte length
/// (e.g., İ U+0130: 2→3 bytes, ẞ U+1E9E: 3→2 bytes).
pub fn escape_xml_tag(content: &str, tag_name: &str) -> String {
    let lower_content = content.to_lowercase();
    let lower_pattern = format!("</{}", tag_name.to_lowercase());

    // Build mapping: lower_content byte offset → content byte offset.
    // Each entry maps a byte position in lower_content to the corresponding
    // byte position in the original content.
    let lower_to_orig: Vec<usize> = {
        let mut map = Vec::with_capacity(lower_content.len() + 1);
        let mut orig_offset = 0usize;
        for orig_char in content.chars() {
            let lowered: String = orig_char.to_lowercase().collect();
            for _ in 0..lowered.len() {
                map.push(orig_offset);
            }
            orig_offset += orig_char.len_utf8();
        }
        map.push(orig_offset); // sentinel for end-of-string
        map
    };

    let mut result = String::with_capacity(content.len() + 32);
    let mut search_from_lower = 0usize;

    while search_from_lower < lower_content.len() {
        match lower_content[search_from_lower..].find(&lower_pattern) {
            None => {
                let orig_start = lower_to_orig[search_from_lower];
                result.push_str(&content[orig_start..]);
                break;
            }
            Some(rel_pos) => {
                let match_lower = search_from_lower + rel_pos;
                let orig_before = lower_to_orig[search_from_lower];
                let orig_match = lower_to_orig[match_lower];
                result.push_str(&content[orig_before..orig_match]);

                // Find closing '>' in the ORIGINAL content after the pattern
                let lower_pat_end = match_lower + lower_pattern.len();
                let orig_pat_end = lower_to_orig[lower_pat_end.min(lower_to_orig.len() - 1)];
                let after_tag_orig = &content[orig_pat_end..];
                let close_orig = after_tag_orig
                    .find('>')
                    .map(|p| p + 1)
                    .unwrap_or(after_tag_orig.len());

                result.push_str(&format!("&lt;/{tag_name}&gt;"));

                // Advance search_from_lower past the '>' in lower_content space
                let target_orig_pos = orig_pat_end + close_orig;
                // Find the lower offset that maps to target_orig_pos
                search_from_lower = lower_to_orig[lower_pat_end..]
                    .iter()
                    .position(|&o| o >= target_orig_pos)
                    .map(|p| lower_pat_end + p)
                    .unwrap_or(lower_content.len());
            }
        }
    }
    result
}

#[cfg(test)]
mod escape_xml_tag_tests {
    use super::escape_xml_tag;

    #[test]
    fn cjk_passthrough() {
        let input = "你好世界 no tags here";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, input);
    }

    #[test]
    fn cjk_with_tag() {
        let input = "你好</soul_content>世界";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, "你好&lt;/soul_content&gt;世界");
    }

    #[test]
    fn case_insensitive_tag() {
        let input = "test</SOUL_CONTENT>end";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, "test&lt;/soul_content&gt;end");
    }

    #[test]
    fn turkish_i_no_panic() {
        // İ (U+0130) lowercases to 3 bytes — tests offset mapping
        let input = "İ</soul_content>test";
        let result = escape_xml_tag(input, "soul_content");
        assert!(result.contains("&lt;/soul_content&gt;"));
        assert!(result.contains("test"));
        assert!(result.starts_with("İ"));
    }

    #[test]
    fn german_eszett_no_panic() {
        // ẞ (U+1E9E) capital sharp S — lowercases to ß (different byte length)
        let input = "straẞe</soul_content>end";
        let result = escape_xml_tag(input, "soul_content");
        assert!(result.contains("&lt;/soul_content&gt;"));
        assert!(result.contains("end"));
    }

    #[test]
    fn no_tag_returns_original() {
        let input = "just some text without any tags";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, input);
    }

    #[test]
    fn multiple_tags() {
        let input = "a</soul_content>b</SOUL_CONTENT>c";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, "a&lt;/soul_content&gt;b&lt;/soul_content&gt;c");
    }

    #[test]
    fn empty_input() {
        let result = escape_xml_tag("", "soul_content");
        assert_eq!(result, "");
    }
}
