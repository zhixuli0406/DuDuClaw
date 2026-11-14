//! Exclude known inline reasoning wrappers from model replies.
//! Structured thinking fields belong to their runtime's reasoning parser;
//! this module handles only known markers in answer text, never prose guesses.

/// Opening tag recognised at the start of the answer.
pub const THINK_OPEN: &str = "<think>";
/// Closing tag.
pub const THINK_CLOSE: &str = "</think>";

/// Compatibility entry point; all wrapper detection lives in one place.
pub fn strip_leading_think_block(text: &str) -> String {
    strip_reasoning_prefix(text)
}

/// Remove known reasoning wrappers before text is used as a channel reply.
/// No English-prose guessing: unmarked reasoning cannot be identified safely.
pub fn strip_reasoning_prefix(text: &str) -> String {
    let mut remaining = text;
    loop {
        let trimmed = remaining.trim_start();
        let mut stripped = false;
        for (open, close) in [
            ("<think>", "</think>"),
            ("<thinking>", "</thinking>"),
            ("<reasoning>", "</reasoning>"),
        ] {
            if let Some(body) = trimmed.strip_prefix(open) {
                remaining = body.split_once(close).map(|(_, answer)| answer.trim_start()).unwrap_or("");
                stripped = true;
                break;
            }
        }
        if stripped { continue; }
        // Harmony channels are structural markers, never prose keywords.
        for (analysis, final_marker) in [
            ("<|start|>assistant<|channel|>analysis<|message|>",
             "<|start|>assistant<|channel|>final<|message|>"),
            ("<|channel|>analysis<|message|>", "<|channel|>final<|message|>"),
            ("<|im_start|>assistant<|meta_sep|>analysis<|im_sep|>",
             "<|im_start|>assistant<|meta_sep|>final<|im_sep|>"),
        ] {
            if let Some(body) = trimmed.strip_prefix(analysis) {
                remaining = body.split_once(final_marker).map(|(_, answer)| answer.trim_start()).unwrap_or("");
                stripped = true;
                break;
            }
        }
        if stripped { continue; }
        // Some chat templates supply the opening tag in the prompt. A known
        // orphan closing tag is the only signal available. False positive:
        // an unquoted explanation of a closing tag may lose its prefix.
        // Preserve quoted/code examples and paired tags within normal prose.
        for (open, close) in [
            ("<think>", "</think>"),
            ("<thinking>", "</thinking>"),
            ("<reasoning>", "</reasoning>"),
        ] {
            if let Some((prefix, answer)) = trimmed.split_once(close) {
                if !prefix.contains(open) && !prefix.contains('`')
                    && !prefix.contains('"') && !prefix.contains('\'')
                {
                    remaining = answer.trim_start();
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped { return remaining.to_string(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_a_leading_block() {
        assert_eq!(
            strip_leading_think_block("<think>let me plan</think>\n\n答案是 42。"),
            "答案是 42。"
        );
        assert_eq!(
            strip_leading_think_block("  \n<think>\n多行\n思考\n</think>Hello"),
            "Hello"
        );
        // No leading block ⇒ unchanged, whitespace included.
        assert_eq!(
            strip_leading_think_block("  plain answer "),
            "  plain answer "
        );
        // A tag later in the text is ordinary content.
        let mid = "The tag <think> starts a block; </think> ends it.";
        assert_eq!(strip_leading_think_block(mid), mid);
        // A tag embedded in visible prose is preserved.
        assert_eq!(
            strip_leading_think_block("<think>a</think>x<think>b</think>"),
            "x<think>b</think>"
        );
    }

    #[test]
    fn unterminated_leading_block_leaves_nothing() {
        assert_eq!(
            strip_leading_think_block("<think>still reasoning when cut off"),
            ""
        );
        assert_eq!(strip_leading_think_block("<think></think>"), "");
    }

    #[test]
    fn cjk_and_emoji_around_the_tags_are_safe() {
        assert_eq!(
            strip_leading_think_block("<think>思考🙂中</think>回覆🙂"),
            "回覆🙂"
        );
    }
}

#[cfg(test)]
mod reasoning_prefix_tests {
    use super::strip_reasoning_prefix as strip;

    #[test]
    fn known_wrappers_and_truncation() {
        for tag in ["think", "thinking", "reasoning"] {
            assert_eq!(strip(&format!(" <{tag}>秘密🙂</{tag}>回答中文🙂")), "回答中文🙂");
            assert_eq!(strip(&format!("秘密🙂</{tag}>回答中文🙂")), "回答中文🙂");
            assert_eq!(strip(&format!("<{tag}>秘密🙂")), "");
        }
        assert_eq!(strip("<think>a</think><reasoning>b</reasoning>答案"), "答案");
        assert_eq!(strip("<|start|>assistant<|channel|>analysis<|message|>secret<|end|><|start|>assistant<|channel|>final<|message|>答案"), "答案");
        assert_eq!(strip("<|channel|>analysis<|message|>secret"), "");
    }

    #[test]
    fn ordinary_prose_and_code_are_preserved() {
        for text in ["Let me check your order.", "  中文🙂答案 ",
            "Example: `<think>x</think>`", "```xml\n</think>\n```",
            "The tag <think> starts a block; </think> ends it."] {
            assert_eq!(strip(text), text);
        }
    }
}

/// Prefer a runtime's final result; only a missing result permits fallback.
/// Sanitizing a thinking-only result must not revive earlier narration.
pub fn final_reply_text(result: &str, assistant: &str, allow_fallback: bool) -> String {
    let selected = if result.trim().is_empty() && allow_fallback { assistant } else { result };
    strip_reasoning_prefix(selected).trim().to_string()
}

/// Last text segment of one assistant event. Thinking and tool blocks never
/// contribute to the fallback answer; an event without text clears it.
pub fn last_assistant_text(blocks: &[serde_json::Value]) -> String {
    blocks.iter().filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"))
        .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
        .last().map(strip_reasoning_prefix).unwrap_or_default()
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn result_priority_and_public_fallback_suppression() {
        assert_eq!(final_reply_text("<thinking>秘密</thinking>答案🙂", "earlier narration", true), "答案🙂");
        assert_eq!(final_reply_text("<thinking>秘密</thinking>", "earlier narration", true), "");
        assert_eq!(final_reply_text("", "last answer🙂", true), "last answer🙂");
        assert_eq!(final_reply_text("", "Let me check", false), "");
    }

    #[test]
    fn only_last_text_segment_and_no_thinking() {
        assert_eq!(last_assistant_text(&[
            json!({"type":"text","text":"Let me check"}),
            json!({"type":"thinking","thinking":"private"}),
            json!({"type":"text","text":"<think>秘密</think>答案🙂"}),
        ]), "答案🙂");
        assert_eq!(last_assistant_text(&[json!({"type":"thinking","thinking":"private"})]), "");
        assert_eq!(last_assistant_text(&[
            json!({"type":"text","text":"earlier narration"}),
            json!({"type":"text","text":"<thinking>秘密</thinking>"}),
        ]), "");
    }
}
