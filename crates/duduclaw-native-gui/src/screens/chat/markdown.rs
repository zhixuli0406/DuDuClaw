// S4b — a small, self-written markdown parser + gpui renderer for agent chat
// bubbles (`screens/chat.rs`'s "P4a honest scope cut #1": plain text only,
// no bold/italic/code/lists/links).
//
// ── Why hand-written instead of `pulldown-cmark` ─────────────────────────
// `pulldown-cmark` (MIT) would still leave 100% of the gpui rendering work
// to write by hand — it only produces an `Event` stream (start/end tags +
// text), not gpui elements, so pulling it in would not skip the part that
// actually takes the effort here. The syntax subset this pass needs (bold /
// italic / inline code / fenced code blocks / unordered lists / links) is
// narrow and well-understood, and a self-contained parser is directly
// unit-testable as plain data (`Block`/`Inline`, zero gpui dependency) the
// same way `text_engine.rs` keeps its own parsing logic gpui-free for
// testability. Matches this crate's own precedent of hand-rolling rather
// than vendoring for a narrow, well-scoped need (see `mds_gpui/mod.rs`'s D4
// spike write-up rejecting a whole design-system crate for 9 components).
//
// ── Scope cuts (honest stubs, not oversights) ─────────────────────────────
//   - Ordered lists, headings, blockquotes, horizontal rules, tables, images,
//     strikethrough: not parsed — any such source line falls through to a
//     plain paragraph, verbatim. Not a panic, just no special rendering.
//   - Links render as a styled (colored) text span but are NOT clickable —
//     the task brief explicitly allows "點擊開瀏覽器可留 stub"; wiring
//     `open::that(url)` or gpui's window-open API is left for a follow-up.
//   - Inline styling wraps at SEGMENT boundaries, not word-by-word merged
//     with surrounding plain text — gpui has no single-text-run "rich text
//     with mixed styles" primitive this pass reaches for (that would mean
//     building a custom `Element` over `TextRun`s); each `**bold**`/`` `code` ``
//     run is one flex item inside a `flex_wrap()` row, so wrapping happens
//     per-run rather than per-glyph. Visually correct for the common case
//     (a styled word/phrase surrounded by plain text), imperfect only for a
//     styled run that itself needs to break mid-run across a line boundary.
//   - `.italic()` depends on gpui's font backend synthesizing an oblique
//     from the single upright `InterVariable.ttf` this crate bundles (S3) —
//     no separate italic font file is bundled, so the visual weight of
//     "italic" is whatever cosmic-text's fake-italic shear produces.
//   - Mono font is `SF Mono` (a system font, not bundled) — a deliberate
//     macOS-only choice matching this task's own scope ("gpui、Mac 可編譯");
//     a cross-platform pass would need to bundle a monospace face the same
//     way `assets/fonts/` bundles Inter/Noto Sans TC.

use gpui::{div, prelude::*, px, AnyElement, Div, FontWeight, SharedString};

use crate::theme;

/// Hard cap on markdown parsing input size. Without it, an adversarial (or
/// just very large) message could push the naive marker-scanning parser
/// below into O(n²) territory (each unmatched `*`/`` ` `` scans forward
/// through the rest of the string looking for a partner that never comes).
/// Beyond this cap the message still renders — as plain text, via
/// [`render_markdown`]'s own fallback — never blocks the UI thread trying to
/// find non-existent structure in tens of thousands of characters.
const MAX_MARKDOWN_CHARS: usize = 20_000;

/// Recursion depth cap for nested inline spans (`**bold with `code`**`).
/// Bounds stack depth against a pathologically deep-nested input
/// (`"*a*a*a*a..."`) — each level of [`parse_inline`] recursion consumes at
/// least one marker pair, so real-world markdown never gets remotely close
/// to this; only a deliberately adversarial message would.
const MAX_INLINE_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Code(String),
    Link { text: String, url: String },
}

// `CodeBlock` ending in the enum's own name (`Block`) trips clippy's
// `enum_variant_names` — kept anyway: it's the standard CommonMark term
// ("fenced code block"), and shortening it to `Code` would collide in
// meaning with `Inline::Code` (a different enum, inline code SPANS) right
// above, which is a worse ambiguity than the lint.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum Block {
    Paragraph(Vec<Inline>),
    CodeBlock { lang: Option<String>, code: String },
    UnorderedList(Vec<Vec<Inline>>),
}

/// Parse markdown source into a block list. Never panics on any input —
/// unterminated fences/markers gracefully degrade to literal text (see this
/// module's doc comment).
pub fn parse(input: &str) -> Vec<Block> {
    if input.chars().count() > MAX_MARKDOWN_CHARS {
        return vec![Block::Paragraph(vec![Inline::Text(input.to_string())])];
    }

    let lines: Vec<&str> = input.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    let mut para_buf: Vec<&str> = Vec::new();
    let mut list_buf: Vec<&str> = Vec::new();

    macro_rules! flush_para {
        () => {
            if !para_buf.is_empty() {
                let text = para_buf.join("\n");
                blocks.push(Block::Paragraph(parse_inline(&text.chars().collect::<Vec<_>>(), 0)));
                para_buf.clear();
            }
        };
    }
    macro_rules! flush_list {
        () => {
            if !list_buf.is_empty() {
                let items: Vec<Vec<Inline>> = list_buf
                    .iter()
                    .map(|item| parse_inline(&item.chars().collect::<Vec<_>>(), 0))
                    .collect();
                blocks.push(Block::UnorderedList(items));
                list_buf.clear();
            }
        };
    }

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        if is_fence_line(trimmed) {
            flush_para!();
            flush_list!();
            let lang = fence_lang(trimmed);
            let mut code_lines: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !is_fence_line(lines[i].trim_start()) {
                code_lines.push(lines[i]);
                i += 1;
            }
            // `i` now points at the closing fence, or `lines.len()` if the
            // fence was never closed (message got cut off mid-block) — both
            // cases just stop collecting; an unterminated fence is not an
            // error, the code block simply runs to the end of the message.
            if i < lines.len() {
                i += 1; // consume the closing fence
            }
            blocks.push(Block::CodeBlock { lang, code: code_lines.join("\n") });
            continue;
        }

        if trimmed.is_empty() {
            flush_para!();
            flush_list!();
            i += 1;
            continue;
        }

        if let Some(item) = unordered_list_item(trimmed) {
            flush_para!();
            list_buf.push(item);
            i += 1;
            continue;
        }

        flush_list!();
        para_buf.push(line);
        i += 1;
    }
    flush_para!();
    flush_list!();

    blocks
}

fn is_fence_line(trimmed: &str) -> bool {
    trimmed.starts_with("```")
}

/// The language tag after an opening fence (e.g. ` ```rust ` → `Some("rust")`).
/// Empty/whitespace-only tag → `None`.
fn fence_lang(trimmed: &str) -> Option<String> {
    let rest = trimmed.trim_start_matches('`').trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// `- item` / `* item` / `+ item` (CommonMark's three bullet markers),
/// tolerating up to 3 leading spaces (already stripped by the caller's
/// `trim_start` on the whole line — this only re-checks the marker itself).
fn unordered_list_item(trimmed: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

/// Find the char-index of the next occurrence of a two-char marker (`**`/`__`)
/// at or after `from`. Char-index based (never byte-index) — CJK-safe per
/// this crate's own convention (`crate::theme`'s doc comment references the
/// same discipline; see also the root `CLAUDE.md` "no raw byte slicing" rule).
fn find_pair(chars: &[char], from: usize, marker: char) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == marker && chars[j + 1] == marker {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn find_single(chars: &[char], from: usize, marker: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == marker)
}

/// Parse `[text](url)` starting at `chars[i] == '['`. Returns the parsed
/// link plus the index just past the closing `)` on success; `None` (and no
/// side effects) on any malformed shape — the caller then falls through to
/// treating `[` as a literal character, never panicking on a dangling
/// bracket.
fn try_parse_link(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let close_bracket = find_single(chars, i + 1, ']')?;
    if close_bracket + 1 >= chars.len() || chars[close_bracket + 1] != '(' {
        return None;
    }
    let close_paren = find_single(chars, close_bracket + 2, ')')?;
    let text: String = chars[i + 1..close_bracket].iter().collect();
    let url: String = chars[close_bracket + 2..close_paren].iter().collect();
    Some((text, url, close_paren + 1))
}

fn parse_inline(chars: &[char], depth: usize) -> Vec<Inline> {
    if depth >= MAX_INLINE_DEPTH {
        return vec![Inline::Text(chars.iter().collect())];
    }

    let mut out: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        // Bold: `**...**` or `__...__`.
        if (chars[i] == '*' || chars[i] == '_') && i + 1 < n && chars[i + 1] == chars[i] {
            let marker = chars[i];
            if let Some(end) = find_pair(chars, i + 2, marker) {
                if !buf.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut buf)));
                }
                out.push(Inline::Bold(parse_inline(&chars[i + 2..end], depth + 1)));
                i = end + 2;
                continue;
            }
        }
        // Inline code: `` `...` `` — content is literal, never re-parsed
        // (CommonMark's own rule: no nested emphasis inside code spans).
        if chars[i] == '`' {
            if let Some(end) = find_single(chars, i + 1, '`') {
                if !buf.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut buf)));
                }
                out.push(Inline::Code(chars[i + 1..end].iter().collect()));
                i = end + 1;
                continue;
            }
        }
        // Link: `[text](url)`.
        if chars[i] == '[' {
            if let Some((text, url, next_i)) = try_parse_link(chars, i) {
                if !buf.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut buf)));
                }
                out.push(Inline::Link { text, url });
                i = next_i;
                continue;
            }
        }
        // Italic: single `*...*` or `_..._` (checked after the doubled-marker
        // bold branch above, so `**` is never mis-read as two empty italics).
        if chars[i] == '*' || chars[i] == '_' {
            let marker = chars[i];
            if let Some(end) = find_single(chars, i + 1, marker) {
                if end > i + 1 {
                    if !buf.is_empty() {
                        out.push(Inline::Text(std::mem::take(&mut buf)));
                    }
                    out.push(Inline::Italic(parse_inline(&chars[i + 1..end], depth + 1)));
                    i = end + 1;
                    continue;
                }
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    if !buf.is_empty() {
        out.push(Inline::Text(buf));
    }
    out
}

// ── Rendering ──────────────────────────────────────────────────────────

/// macOS system monospace — see this module's doc comment on why this isn't
/// a bundled font.
fn mono_font() -> gpui::Font {
    gpui::Font {
        fallbacks: Some(gpui::FontFallbacks::from_fonts(vec!["Menlo".to_string(), "Courier New".to_string()])),
        ..gpui::font("SF Mono")
    }
}

/// Parse + render `text` as a column of blocks. The single entry point
/// `screens/chat.rs`'s message bubble calls for an assistant reply.
pub fn render_markdown(text: &str) -> Div {
    let blocks = parse(text);
    let mut container = div().flex().flex_col().gap_2();
    for block in &blocks {
        container = container.child(render_block(block));
    }
    container
}

fn render_block(block: &Block) -> AnyElement {
    match block {
        Block::Paragraph(inlines) => render_paragraph(inlines).into_any(),
        Block::CodeBlock { lang, code } => render_code_block(lang.as_deref(), code).into_any(),
        Block::UnorderedList(items) => render_list(items).into_any(),
    }
}

fn render_paragraph(inlines: &[Inline]) -> Div {
    div().flex().flex_wrap().items_baseline().children(inlines.iter().map(render_inline))
}

fn render_inline(seg: &Inline) -> AnyElement {
    match seg {
        Inline::Text(s) => div().child(SharedString::from(s.clone())).into_any(),
        Inline::Bold(inner) => {
            div().flex().flex_wrap().font_weight(FontWeight::BOLD).children(inner.iter().map(render_inline)).into_any()
        }
        Inline::Italic(inner) => {
            div().flex().flex_wrap().italic().children(inner.iter().map(render_inline)).into_any()
        }
        Inline::Code(s) => code_span(s).into_any(),
        // Stub: styled but not clickable — see this module's doc comment.
        Inline::Link { text, url: _ } => {
            div().text_color(theme::alpha(theme::INFO, 1.0)).child(SharedString::from(text.clone())).into_any()
        }
    }
}

fn code_span(code: &str) -> Div {
    div()
        .px_1()
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::alpha(theme::MUTED, 0.6))
        .font(mono_font())
        .text_size(px(theme::TEXT_XS))
        .child(SharedString::from(code.to_string()))
}

fn render_code_block(lang: Option<&str>, code: &str) -> Div {
    let mut block = div()
        .flex()
        .flex_col()
        .gap_1()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::MUTED, 0.5))
        .p_2();
    if let Some(lang) = lang.filter(|l| !l.is_empty()) {
        block = block.child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(lang.to_string())),
        );
    }
    block.child(
        div()
            .font(mono_font())
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(SharedString::from(code.to_string())),
    )
}

fn render_list(items: &[Vec<Inline>]) -> Div {
    let mut list = div().flex().flex_col().gap_1();
    for item in items {
        list = list.child(
            div()
                .flex()
                .gap_2()
                .child(div().text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("•"))
                .child(div().flex().flex_wrap().items_baseline().children(item.iter().map(render_inline))),
        );
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(blocks: &[Block]) -> &Vec<Inline> {
        match &blocks[0] {
            Block::Paragraph(inlines) => inlines,
            other => panic!("expected a Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn plain_text_is_one_text_segment() {
        let blocks = parse("hello world");
        assert_eq!(blocks.len(), 1);
        assert_eq!(plain(&blocks), &vec![Inline::Text("hello world".to_string())]);
    }

    #[test]
    fn bold_stars() {
        let blocks = parse("a **bold** b");
        assert_eq!(
            plain(&blocks),
            &vec![
                Inline::Text("a ".to_string()),
                Inline::Bold(vec![Inline::Text("bold".to_string())]),
                Inline::Text(" b".to_string()),
            ]
        );
    }

    #[test]
    fn bold_underscores() {
        let blocks = parse("__bold__");
        assert_eq!(plain(&blocks), &vec![Inline::Bold(vec![Inline::Text("bold".to_string())])]);
    }

    #[test]
    fn italic_star_and_underscore() {
        assert_eq!(plain(&parse("*it*")), &vec![Inline::Italic(vec![Inline::Text("it".to_string())])]);
        assert_eq!(plain(&parse("_it_")), &vec![Inline::Italic(vec![Inline::Text("it".to_string())])]);
    }

    #[test]
    fn inline_code() {
        let blocks = parse("run `cargo build` now");
        assert_eq!(
            plain(&blocks),
            &vec![
                Inline::Text("run ".to_string()),
                Inline::Code("cargo build".to_string()),
                Inline::Text(" now".to_string()),
            ]
        );
    }

    #[test]
    fn link() {
        let blocks = parse("see [docs](https://example.com/x) here");
        assert_eq!(
            plain(&blocks),
            &vec![
                Inline::Text("see ".to_string()),
                Inline::Link { text: "docs".to_string(), url: "https://example.com/x".to_string() },
                Inline::Text(" here".to_string()),
            ]
        );
    }

    #[test]
    fn unordered_list_dash() {
        let blocks = parse("- one\n- two\n- three");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], vec![Inline::Text("one".to_string())]);
                assert_eq!(items[2], vec![Inline::Text("three".to_string())]);
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn unordered_list_star_and_plus_markers() {
        let blocks = parse("* a\n+ b");
        match &blocks[0] {
            Block::UnorderedList(items) => assert_eq!(items.len(), 2),
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn fenced_code_block_with_lang() {
        let blocks = parse("```rust\nfn main() {}\n```");
        assert_eq!(blocks, vec![Block::CodeBlock { lang: Some("rust".to_string()), code: "fn main() {}".to_string() }]);
    }

    #[test]
    fn fenced_code_block_without_lang() {
        let blocks = parse("```\nplain\n```");
        assert_eq!(blocks, vec![Block::CodeBlock { lang: None, code: "plain".to_string() }]);
    }

    #[test]
    fn unterminated_fence_runs_to_end_of_message_no_panic() {
        let blocks = parse("```python\nprint(1)\nstill going");
        assert_eq!(
            blocks,
            vec![Block::CodeBlock { lang: Some("python".to_string()), code: "print(1)\nstill going".to_string() }]
        );
    }

    #[test]
    fn paragraphs_separated_by_blank_line() {
        let blocks = parse("first\n\nsecond");
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn nested_code_inside_bold_no_panic() {
        let blocks = parse("**bold `code` mix**");
        assert_eq!(
            plain(&blocks),
            &vec![Inline::Bold(vec![
                Inline::Text("bold ".to_string()),
                Inline::Code("code".to_string()),
                Inline::Text(" mix".to_string()),
            ])]
        );
    }

    #[test]
    fn unterminated_bold_marker_degrades_to_literal_text_no_panic() {
        let blocks = parse("this is **not closed");
        assert_eq!(plain(&blocks), &vec![Inline::Text("this is **not closed".to_string())]);
    }

    #[test]
    fn unterminated_inline_code_degrades_to_literal_backtick_no_panic() {
        let blocks = parse("oops `never closes");
        assert_eq!(plain(&blocks), &vec![Inline::Text("oops `never closes".to_string())]);
    }

    #[test]
    fn dangling_open_bracket_is_literal_not_a_link() {
        let blocks = parse("weird [bracket without the rest");
        assert_eq!(plain(&blocks), &vec![Inline::Text("weird [bracket without the rest".to_string())]);
    }

    #[test]
    fn malformed_link_missing_paren_is_literal() {
        let blocks = parse("[text] no paren");
        assert_eq!(plain(&blocks), &vec![Inline::Text("[text] no paren".to_string())]);
    }

    #[test]
    fn cjk_content_does_not_panic_and_round_trips() {
        let blocks = parse("你好 **粗體中文** 結束，還有 `代碼片段` 喔");
        assert_eq!(
            plain(&blocks),
            &vec![
                Inline::Text("你好 ".to_string()),
                Inline::Bold(vec![Inline::Text("粗體中文".to_string())]),
                Inline::Text(" 結束，還有 ".to_string()),
                Inline::Code("代碼片段".to_string()),
                Inline::Text(" 喔".to_string()),
            ]
        );
    }

    #[test]
    fn emoji_and_multibyte_grapheme_input_does_not_panic() {
        // Emoji + combining marks are exactly the class of input that panics
        // a byte-index slicer (coding convention #1) — this parser only ever
        // indexes a `Vec<char>`, never raw bytes.
        let blocks = parse("🐾 **bold 🎉** café naïve 👨‍👩‍👧‍👦");
        assert!(!blocks.is_empty());
    }

    #[test]
    fn pathological_unmatched_markers_do_not_panic() {
        let input = "*".repeat(500) + &"`".repeat(500);
        let blocks = parse(&input);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn deeply_nested_markers_do_not_stack_overflow() {
        // Each level wraps the next in one more `*...*` — without the depth
        // cap this recurses ~5000 levels deep in `parse_inline`.
        let mut s = "x".to_string();
        for _ in 0..5000 {
            s = format!("*{s}*");
        }
        let blocks = parse(&s);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn oversized_input_falls_back_to_plain_text_without_hanging() {
        let input = "*".repeat(MAX_MARKDOWN_CHARS + 1);
        let blocks = parse(&input);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(inlines) => {
                assert_eq!(inlines, &vec![Inline::Text(input)]);
            }
            other => panic!("expected a single literal Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn empty_input_produces_no_blocks() {
        assert_eq!(parse(""), Vec::new());
    }

    #[test]
    fn whitespace_only_input_produces_no_blocks() {
        assert_eq!(parse("   \n\n  \n"), Vec::new());
    }

    #[test]
    fn render_markdown_does_not_panic_on_every_case_above() {
        // Smoke-test the renderer (pure `div()` tree construction, no gpui
        // App/window needed — see this module's doc comment) against every
        // shape exercised above, so a parser change that breaks the
        // renderer's `match` exhaustiveness is caught here too.
        for input in [
            "plain",
            "**bold**",
            "*it*",
            "`code`",
            "[text](url)",
            "- a\n- b",
            "```rust\ncode\n```",
            "**bold `code` mix**",
            "unterminated **bold",
            "🐾 **bold 🎉** café",
        ] {
            let _ = render_markdown(input);
        }
    }
}
