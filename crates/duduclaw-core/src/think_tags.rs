//! Strip a leading `<think>…</think>` block from a model's answer text.
//!
//! Open-weight reasoning models served through an OpenAI-compatible endpoint
//! (Qwen3, DeepSeek-R1 distills, many llama.cpp / vLLM setups) often put
//! their reasoning inline in `content`, wrapped in `<think>` tags, instead of
//! a separate `reasoning_content` field. Without this, that reasoning was
//! shown to the user as part of the reply.
//!
//! Deliberately narrow: only a block at the very start of the answer (after
//! leading whitespace) is removed, matching how these models emit it. A
//! `<think>` that appears later is ordinary text (a user may legitimately
//! ask about the tag) and is left alone. An unterminated leading block means
//! the model never got past its reasoning, so nothing is left to show.
//!
//! Known limitation: some chat templates put the opening `<think>` in the
//! prompt, so the model's output starts mid-reasoning and only carries the
//! closing `…</think>`. That shape is not detected here (it is
//! indistinguishable from an answer that mentions the tag) and still needs
//! the server to split reasoning out (`reasoning_content`).

/// Opening tag recognised at the start of the answer.
pub const THINK_OPEN: &str = "<think>";
/// Closing tag.
pub const THINK_CLOSE: &str = "</think>";

/// The answer with a leading `<think>` block removed. Text without one is
/// returned unchanged (byte-identical, including surrounding whitespace).
pub fn strip_leading_think_block(text: &str) -> String {
    let trimmed = text.trim_start();
    let Some(after_open) = trimmed.strip_prefix(THINK_OPEN) else {
        return text.to_string();
    };
    match after_open.find(THINK_CLOSE) {
        Some(end) => after_open[end + THINK_CLOSE.len()..]
            .trim_start()
            .to_string(),
        None => String::new(),
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
        // Only the first block goes; a second one is the model's own text.
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
