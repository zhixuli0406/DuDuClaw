//! Strip AI-runtime *internal* messages out of user-facing replies.
//!
//! **WP11-A (2026-08-04, Joanna field report).** A Telegram reply reached the
//! customer containing Claude Code's own terminal chrome:
//!
//! ```text
//! ⚠ Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker · restart
//!   with CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 to keep future transcripts
//! ⏵⏵ manual mode on (shift+tab to cycle)
//! paste again to expand
//! ```
//!
//! Those strings are painted by the interactive CLI TUI (verified live against
//! `claude` 2.1.220 — the test fixtures below are byte-for-byte excerpts of a
//! real PTY capture). `duduclaw-cli-runtime`'s chrome filter already drops
//! *some* of them, but its marker lists are enumerated per TUI release and lag
//! behind new notice lines; and every other runtime (codex / gemini /
//! antigravity) paints its own chrome.
//!
//! So this module is the **last line of defence at the shared reply-assembly
//! point** — every channel funnels through `channel_reply::build_reply_*` and
//! `dispatcher::forward_to_channel`, so two hooks cover all of them.
//!
//! # Decision rule: a line must LOOK LIKE CHROME, not merely mention it
//!
//! Deleting a user's content is far worse than leaking a line of chrome, so
//! pattern-matching alone is **not** sufficient to remove anything. A line is
//! removed only when it matches a pattern **and** produces independent
//! evidence that it came from a terminal rather than from the model:
//!
//! | Evidence | Meaning |
//! |---|---|
//! | (a) notice glyph | the line carries a TUI status glyph (`⚠ ⏵ ⎿ ·` …) |
//! | (b) glued render | a multi-word marker appears space-less (`pasteagaintoexpand`) — impossible in prose, see WP11-B |
//! | (c) trailing status | the line sits in the last [`TAIL_WINDOW`] lines, is short, and does not end like a sentence |
//!
//! …and none of the hard vetoes fire:
//!
//! - **(d)** the line is inside a fenced code block (```) — never touched;
//! - **(e)** the line contains CJK / kana / Hangul — assumed to be agent prose;
//! - **(f)** patterns whose wording is common in ordinary answers are marked
//!   [`NoisePattern::strict`] and cannot be removed on (c) alone.
//!
//! Anything that matches but fails these tests is **kept** and logged at `warn`
//! so the case can be reviewed instead of silently disappearing. If filtering
//! would empty a non-empty reply, the original text is returned unchanged — a
//! silent empty reply is a worse failure than visible noise.
//!
//! Matching is whitespace-insensitive on purpose: the TUI positions text with
//! cursor-move escapes rather than literal spaces, so the same notice arrives
//! as either `"Transcript saving is off"` or `"Transcriptsavingisoff"`
//! depending on whether the terminal painted it fresh or diff-repainted it.

use tracing::warn;

/// Longest span we are willing to delete as noise. Real notices are one short
/// terminal line; anything longer is treated as content.
const MAX_NOISE_SPAN_CHARS: usize = 200;

/// How many trailing lines count as the "status footer" region for rule (c).
const TAIL_WINDOW: usize = 3;

/// Rule (c) additionally caps line length — a terminal status line is short.
///
/// In practice almost every pattern is [`NoisePattern::strict`], because a
/// genuine support answer can use the same wording; rule (c) is therefore a
/// narrow fallback rather than the main gate. That is deliberate: every TUI
/// notice observed in the 2026-08-04 live capture carried either a notice
/// glyph (a) or the glued render (b), so requiring one of those loses no real
/// detection while removing a whole class of false positives.
const TAIL_MAX_CHARS: usize = 120;

/// Glyphs the CLI TUI uses to lead a notice / status line. Their presence is
/// evidence (a); they are also where an inline notice is cut from a content
/// line.
const NOTICE_LEAD_GLYPHS: &[char] =
    &['⚠', '⏵', '⏸', '⏭', '⎿', '✳', '✶', '✻', '✽', '●', '·', '⏺'];

/// Sentence-ending punctuation. A line that ends with one of these reads as
/// prose, so rule (c) refuses to treat it as a status footer.
const SENTENCE_ENDINGS: &[char] =
    &['.', '!', '?', ':', ';', ',', '。', '！', '？', '：', '；', '、', '」', '）', ')'];

/// One recognisable class of internal message.
///
/// A line matches when its compact form contains **every** needle in `all_of`
/// and (when `any_of` is non-empty) **at least one** needle from `any_of`.
/// Needles are already compact + lowercase.
struct NoisePattern {
    id: &'static str,
    all_of: &'static [&'static str],
    any_of: &'static [&'static str],
    /// Space-less renders that are themselves proof the line came from a TUI
    /// diff-repaint — multi-word phrases that cannot occur in ordinary prose.
    /// Presence of any of these in the raw (lowercased) line satisfies (b).
    glued_proof: &'static [&'static str],
    /// Wording common enough in genuine answers that trailing position alone
    /// (rule c) is not acceptable evidence — requires (a) or (b).
    strict: bool,
}

const fn p(
    id: &'static str,
    all_of: &'static [&'static str],
    any_of: &'static [&'static str],
    glued_proof: &'static [&'static str],
    strict: bool,
) -> NoisePattern {
    NoisePattern { id, all_of, any_of, glued_proof, strict }
}

/// Pattern classes, not literal sentences — each is meant to catch a whole
/// family of notices, including wordings we have not seen yet.
const PATTERNS: &[NoisePattern] = &[
    // ── Claude Code TUI ────────────────────────────────────────────────
    p(
        "transcript_persistence",
        &["transcript"],
        &["savingisoff", "savingison", "notbeingsaved", "keepfuturetranscripts", "willnotbesaved"],
        &["transcriptsavingisoff", "transcriptssavingisoff", "keepfuturetranscripts"],
        true,
    ),
    p(
        "runtime_env_hint",
        &[],
        &[
            "claude_code_child_session",
            "claude_code_force_session_persistence",
            "claude_code_entrypoint",
            "claude_code_max_output_tokens",
            "codex_sandbox",
            "gemini_cli_",
        ],
        &["inheritedclaude_code_", "restartwithclaude_code_"],
        true,
    ),
    p(
        "runtime_env_restart_hint",
        &["claude_code_"],
        &["restartwith", "restartthe", "=1to", "marker"],
        &["restartwithclaude_code_", "claude_code_child_sessionmarker"],
        true,
    ),
    // "You can paste again to expand" is a sentence a support answer really
    // writes (review counter-example A10), so the spaced form is only removed
    // when it carries a TUI glyph; the glued form is proof on its own.
    p(
        "paste_marker",
        &[],
        &["pasteagaintoexpand", "ctrl+rtoexpand", "pastedtext#", "pastedcontent#"],
        &["pasteagaintoexpand", "ctrl+rtoexpand", "pastedtext#"],
        true,
    ),
    p(
        "mode_footer",
        &["modeon"],
        &["shift+tabtocycle", "⏵", "⏸", "⏭", "ⅱ"],
        &["modeon(shift+tabtocycle)", "automodeon", "manualmodeon", "acceptedistmodeon"],
        false,
    ),
    p("permission_footer", &["bypasspermissions"], &[], &["bypasspermissionsmodeon"], true),
    // Spinner lines also carry "esc to interrupt", so they must be classified
    // BEFORE `interrupt_hint` to be reported as what they are. Both are noise;
    // ordering only affects the reported id. The ellipsis is part of the marker
    // so ordinary prose ("thinking about it") cannot match.
    p(
        "spinner_word",
        &[],
        &["thinking…", "pondering…", "inferring…", "recombobulating…", "cookedfor", "esctointerrupt)"],
        &["thinking…", "pondering…", "inferring…", "recombobulating…"],
        true,
    ),
    // Wording that legitimately appears in help answers ⇒ strict.
    p(
        "interrupt_hint",
        &[],
        &["esctointerrupt", "ctrl+ctoexit", "ctrl+gtoeditinvim", "?forshortcuts", "←foragents"],
        &["esctointerrupt", "ctrl+gtoeditinvim", "?forshortcuts", "←foragents", "ctrl+ctoexit"],
        true,
    ),
    p(
        "mcp_notice",
        &["mcpserver"],
        &["needsauthentication", "needsauth", "failedtoconnect", "failed·", "run/mcp"],
        &["mcpserverneedsauthentication", "mcpserverfailed"],
        true,
    ),
    p(
        "release_banner",
        &[],
        &["run/inittocreate", "/release-notesformore", "whatsnewinclaudecode", "what'snewinclaudecode"],
        &["run/inittocreate", "/release-notesformore"],
        true,
    ),
    // ── P2: updates, billing, compaction, cost, spinners ───────────────
    p(
        "update_notice",
        &[],
        &["newversionavailable", "updateavailable", "npmi-g@anthropic-ai", "npmi-gdudu", "npmi-gclaude"],
        &["newversionavailable", "updateavailable"],
        true,
    ),
    p(
        "usage_limit_notice",
        &[],
        &["creditbalancetoolow", "approachingusagelimit", "usagelimitreached", "limitwillresetat"],
        &["creditbalancetoolow", "approachingusagelimit", "usagelimitreached"],
        true,
    ),
    p("limit_reached_notice", &["limitreached"], &[], &["limitreached·", "5-hourlimitreached"], true),
    p(
        "context_compaction",
        &[],
        &["run/compact", "auto-compact", "autocompact", "contextlowrun/compact", "compactingconversation"],
        &["run/compact", "auto-compact", "contextlowrun/compact"],
        true,
    ),
    p(
        "cost_summary",
        &["totalcost"],
        &["totalduration", "apiduration", "totaltokens", "usd", "$"],
        &["totalcost(usd)", "totaldurationapi"],
        true,
    ),
    // Spinner words: extremely common in prose ⇒ strict, and the ellipsis is
    // part of the marker so "thinking about it" cannot match.
    // ── P2: other runtimes ─────────────────────────────────────────────
    p(
        "codex_notice",
        &[],
        &["toexitpressctrl+c", "readingpromptfromstdin"],
        &["toexitpressctrl+c", "readingpromptfromstdin"],
        true,
    ),
    p(
        "gemini_notice",
        &[],
        &["gemini.mdfile", "datacollectionisdisabled", "tipsforgettingstarted"],
        &["gemini.mdfile", "datacollectionisdisabled", "tipsforgettingstarted"],
        true,
    ),
    p("sandbox_footer", &["nosandbox"], &[], &["(nosandbox)", "nosandbox·"], true),
    p(
        "trust_dialog",
        &[],
        &["doyoutrustthefilesinthisfolder", "doyoutrustthefiles"],
        &["doyoutrustthefiles"],
        true,
    ),
];

/// Compact form used for matching: drop all whitespace, lowercase.
fn compact(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Count of East-Asian script characters — Han (incl. Extension A), kana and
/// Hangul syllables. A line carrying any of these is assumed to be the agent's
/// own prose (veto e).
fn cjk_count(s: &str) -> usize {
    s.chars()
        .filter(|&c| {
            ('\u{4E00}'..='\u{9FFF}').contains(&c)   // CJK Unified Ideographs
                || ('\u{3400}'..='\u{4DBF}').contains(&c) // …Extension A
                || ('\u{3040}'..='\u{30FF}').contains(&c) // Hiragana + Katakana
                || ('\u{AC00}'..='\u{D7A3}').contains(&c) // Hangul syllables
        })
        .count()
}

/// Returns the first pattern `line` matches, if any.
fn classify(line: &str) -> Option<&'static NoisePattern> {
    let c = compact(line);
    if c.is_empty() {
        return None;
    }
    PATTERNS.iter().find(|p| {
        p.all_of.iter().all(|n| c.contains(n))
            && (p.any_of.is_empty() || p.any_of.iter().any(|n| c.contains(n)))
    })
}

/// Evidence (a): the line carries a TUI notice glyph.
fn has_notice_glyph(line: &str) -> bool {
    line.chars().any(|c| NOTICE_LEAD_GLYPHS.contains(&c))
}

/// Evidence (b): a multi-word marker appears in the raw line without its
/// spaces — the signature of a TUI diff-repaint (WP11-B), impossible in prose.
fn has_glued_render(line: &str, pat: &NoisePattern) -> bool {
    if pat.glued_proof.is_empty() {
        return false;
    }
    let lower: String = line.chars().flat_map(char::to_lowercase).collect();
    pat.glued_proof.iter().any(|g| lower.contains(g))
}

/// Evidence (c): a short, non-sentence line sitting in the trailing status
/// region of the reply.
fn is_trailing_status(line: &str, lines_from_end: usize) -> bool {
    if lines_from_end >= TAIL_WINDOW {
        return false;
    }
    let t = line.trim_end();
    if t.chars().count() > TAIL_MAX_CHARS {
        return false;
    }
    !t.chars().last().is_some_and(|c| SENTENCE_ENDINGS.contains(&c))
}

/// Hard vetoes: never delete a span carrying East-Asian script, and never
/// delete anything longer than one terminal line.
fn passes_hard_vetoes(span: &str) -> bool {
    cjk_count(span) == 0 && span.chars().count() <= MAX_NOISE_SPAN_CHARS
}

/// Full decision: may this matched line be removed?
fn may_remove(line: &str, pat: &NoisePattern, lines_from_end: usize, in_fence: bool) -> bool {
    if in_fence || !passes_hard_vetoes(line) {
        return false;
    }
    let glyph = has_notice_glyph(line);
    let glued = has_glued_render(line, pat);
    if pat.strict {
        return glyph || glued;
    }
    glyph || glued || is_trailing_status(line, lines_from_end)
}

/// Outcome of [`strip_cli_noise`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoiseVerdict {
    /// The cleaned text (equal to the input when nothing was removed).
    pub text: String,
    /// Pattern ids removed, in order.
    pub removed: Vec<&'static str>,
    /// Pattern ids that matched but were KEPT because they failed the
    /// evidence tests or a hard veto. Surfaced so operators can widen the
    /// rules deliberately rather than discovering silent deletions.
    pub kept_suspicious: Vec<&'static str>,
}

impl NoiseVerdict {
    fn unchanged(text: &str) -> Self {
        Self { text: text.to_string(), removed: Vec::new(), kept_suspicious: Vec::new() }
    }
}

/// Remove AI-runtime internal messages from a user-facing reply.
///
/// Line-granular, with inline truncation when a notice is glued to the tail of
/// a real content line (the TUI does this when it repaints without a newline).
pub fn strip_cli_noise(text: &str) -> NoiseVerdict {
    if text.is_empty() {
        return NoiseVerdict::unchanged(text);
    }
    // Cheap bail-out: no pattern can match without at least one of these.
    let lower = text.to_lowercase();
    const TRIPWIRES: &[&str] = &[
        "transcript", "claude_code_", "codex_sandbox", "gemini_cli_", "paste", "mode on",
        "modeon", "bypass permissions", "bypasspermissions", "interrupt", "shortcuts",
        "mcp server", "mcpserver", "release-notes", "for agents", "foragents", "vim",
        "version available", "versionavailable", "npm i -g", "npmi-g", "limit", "credit balance",
        "creditbalance", "compact", "total cost", "totalcost", "thinking…", "pondering…",
        "inferring…", "recombobulating", "ctrl+c", "stdin", "gemini.md", "sandbox",
        "data collection", "datacollection", "getting started", "gettingstarted",
        "trust the files", "trustthefiles", "cooked for", "cookedfor", "what's new", "whatsnew",
        "/init",
    ];
    if !TRIPWIRES.iter().any(|t| lower.contains(t)) {
        return NoiseVerdict::unchanged(text);
    }

    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let mut removed: Vec<&'static str> = Vec::new();
    let mut kept: Vec<&'static str> = Vec::new();
    let mut out_lines: Vec<String> = Vec::with_capacity(total);
    // Veto (d): fenced code blocks are content, never chrome.
    let mut in_fence = false;

    for (idx, line) in lines.iter().enumerate() {
        let is_fence_marker = line.trim_start().starts_with("```");
        if is_fence_marker {
            in_fence = !in_fence;
            out_lines.push((*line).to_string());
            continue;
        }
        let lines_from_end = total - 1 - idx;
        let Some(pat) = classify(line) else {
            out_lines.push((*line).to_string());
            continue;
        };
        if may_remove(line, pat, lines_from_end, in_fence) {
            removed.push(pat.id);
            continue;
        }
        // Inline truncation: a notice glued to the tail of a real content line.
        // The cut always starts at a notice glyph, so evidence (a) holds for
        // the removed span by construction; the vetoes still apply to it.
        let mut cut: Option<usize> = None;
        if !in_fence {
            for (byte_idx, ch) in line.char_indices() {
                if !NOTICE_LEAD_GLYPHS.contains(&ch) {
                    continue;
                }
                let suffix = &line[byte_idx..];
                let Some(spat) = classify(suffix) else { continue };
                if !passes_hard_vetoes(suffix) {
                    continue;
                }
                if line[..byte_idx].trim().is_empty() {
                    break; // whole line is the notice; handled above
                }
                cut = Some(byte_idx);
                removed.push(spat.id);
                break;
            }
        }
        match cut {
            Some(byte_idx) => out_lines.push(line[..byte_idx].trim_end().to_string()),
            None => {
                // Matched but not provably chrome — KEEP it and say so.
                kept.push(pat.id);
                out_lines.push((*line).to_string());
            }
        }
    }

    if removed.is_empty() {
        if !kept.is_empty() {
            warn!(
                patterns = ?kept,
                "cli_noise: internal-message pattern matched but the line did not look like \
                 terminal chrome — reply kept verbatim"
            );
        }
        return NoiseVerdict { text: text.to_string(), removed, kept_suspicious: kept };
    }

    let cleaned = collapse_blank_runs(&out_lines);
    if cleaned.trim().is_empty() {
        // Never turn a non-empty reply into silence.
        warn!(
            patterns = ?removed,
            "cli_noise: filtering would empty the reply — returning original text unchanged"
        );
        return NoiseVerdict { text: text.to_string(), removed: Vec::new(), kept_suspicious: removed };
    }
    warn!(patterns = ?removed, "cli_noise: stripped AI-runtime internal message(s) from outgoing reply");
    NoiseVerdict { text: cleaned, removed, kept_suspicious: kept }
}

/// Join kept lines, collapsing runs of blank lines left behind by removals and
/// trimming the leading/trailing blank edges.
fn collapse_blank_runs(lines: &[String]) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut prev_blank = true; // suppresses leading blanks
    for l in lines {
        let blank = l.trim().is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push(l.as_str());
        prev_blank = blank;
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-for-byte excerpt of a live `claude` 2.1.220 PTY capture taken on
    /// 2026-08-04 (ANSI already stripped) — the spaced render form.
    const LIVE_TRANSCRIPT_NOTICE: &str =
        "⚠ Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker · restart \
         with CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 to keep future transcripts";
    /// The same notice as it reached the customer: the TUI's diff-repaint
    /// expresses horizontal spacing with cursor-move escapes, so once those are
    /// stripped the ASCII spaces are gone.
    const FIELD_TRANSCRIPT_NOTICE: &str =
        "⚠Transcriptssavingisoff—inheritedCLAUDE_CODE_CHILD_SESSIONmarker·restartwithCLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1tokeepfuturetranscripts";

    // ── Positive: the real field noise must still be caught ─────────────

    #[test]
    fn removes_transcript_notice_in_both_render_forms() {
        for notice in [LIVE_TRANSCRIPT_NOTICE, FIELD_TRANSCRIPT_NOTICE] {
            let input = format!("好的，我幫你查到三筆資料。\n{notice}");
            let v = strip_cli_noise(&input);
            assert_eq!(v.text, "好的，我幫你查到三筆資料。", "notice not removed: {notice}");
            assert!(!v.removed.is_empty());
        }
    }

    /// Even buried in the middle of a long reply, the field noise is removed —
    /// it carries both a glyph (a) and the glued render (b), so it never needs
    /// the trailing-position rule.
    #[test]
    fn field_noise_is_removed_even_when_not_trailing() {
        let input = format!(
            "第一段說明。\n{FIELD_TRANSCRIPT_NOTICE}\n第二段說明。\n第三段說明。\n第四段說明。"
        );
        let v = strip_cli_noise(&input);
        assert!(!v.text.contains("CLAUDE_CODE_CHILD_SESSION"));
        assert!(v.text.contains("第二段說明。"));
    }

    #[test]
    fn removes_mode_footer_and_paste_marker() {
        let input = "報告已完成。\n⏵⏵ manual mode on (shift+tab to cycle)\n⎿ paste again to expand";
        let v = strip_cli_noise(input);
        assert_eq!(v.text, "報告已完成。");
        assert!(v.removed.contains(&"mode_footer"));
        assert!(v.removed.contains(&"paste_marker"));
    }

    #[test]
    fn removes_the_space_stripped_field_forms() {
        let input = "完成了。\n⏵⏵manualmodeon(shift+tabtocycle)\npasteagaintoexpand\nesctointerrupt";
        let v = strip_cli_noise(input);
        assert_eq!(v.text, "完成了。");
        assert_eq!(v.removed.len(), 3);
    }

    #[test]
    fn removes_mcp_and_env_notices() {
        let input = "以下是結果：\n⚠ 1 MCP server needs authentication · run /mcp\n⚠ CLAUDE_CODE_ENTRYPOINT=cli";
        let v = strip_cli_noise(input);
        assert_eq!(v.text, "以下是結果：");
    }

    #[test]
    fn truncates_notice_glued_to_content_line() {
        let input = "查詢完成，共 3 筆。 ⚠ Transcript saving is off — restart with CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 to keep future transcripts";
        let v = strip_cli_noise(input);
        assert_eq!(v.text, "查詢完成，共 3 筆。");
        assert_eq!(v.removed, vec!["transcript_persistence"]);
    }

    #[test]
    fn multibyte_inline_cut_is_char_boundary_safe() {
        let input = "完成⚠ Transcript saving is off — restart with CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 to keep future transcripts";
        let v = strip_cli_noise(input);
        assert_eq!(v.text, "完成");
    }

    #[test]
    fn never_empties_a_reply() {
        let v = strip_cli_noise("⏵⏵ manual mode on (shift+tab to cycle)");
        assert_eq!(v.text, "⏵⏵ manual mode on (shift+tab to cycle)");
        assert!(v.removed.is_empty());
        assert_eq!(v.kept_suspicious, vec!["mode_footer"]);
    }

    #[test]
    fn blank_runs_left_by_removal_are_collapsed() {
        let input = "第一段\n\n⏵⏵ auto mode on (shift+tab to cycle)\n\n第二段";
        let v = strip_cli_noise(input);
        assert_eq!(v.text, "第一段\n\n第二段");
    }

    /// P2 pattern classes — each in its authentic glyph-led / glued form.
    #[test]
    fn removes_p2_notice_classes() {
        let cases: &[(&str, &str)] = &[
            ("update_notice", "⚠ New version available · run npm i -g @anthropic-ai/claude-code"),
            ("usage_limit_notice", "⚠ Approaching usage limit · resets at 3pm"),
            ("limit_reached_notice", "⚠ 5-hour limit reached · try again later"),
            ("context_compaction", "⚠ Context low · Run /compact to compact the conversation"),
            ("cost_summary", "· Total cost (USD): $0.42 · Total duration (API): 1m 3s"),
            ("spinner_word", "✻ Thinking… (12s · esc to interrupt)"),
            ("codex_notice", "· To exit press Ctrl+C"),
            ("gemini_notice", "· Using: 1 GEMINI.md file · no sandbox"),
            ("trust_dialog", "⚠ Do you trust the files in this folder?"),
        ];
        for (id, notice) in cases {
            let input = format!("這是給使用者的答案。\n{notice}");
            let v = strip_cli_noise(&input);
            assert_eq!(v.text, "這是給使用者的答案。", "{id} not removed from {notice:?}");
            assert!(v.removed.contains(id), "{id} expected, got {:?}", v.removed);
        }
    }

    // ── Negative: reviewer counter-examples (must NOT be touched) ───────

    /// Every case below is an authentic answer a user could receive. None of
    /// them may lose a character. Labels track the review findings
    /// (B1/B2/B3/B5/B6/B7/B9, A2/A9/A10).
    #[test]
    fn reviewer_counter_examples_are_never_modified() {
        let cases: &[(&str, &str)] = &[
            (
                "B1 English answer explaining transcript saving",
                "I checked the runtime configuration for you.\n\
                 Transcript saving is off by default in headless mode, which is why the log \
                 folder stays empty after each run.\n\
                 Turn it back on if you want the raw conversation kept on disk",
            ),
            (
                "A2 English answer quoting the env var",
                "Here is the answer to your question about persistence.\n\
                 Set CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 before you start the process and \
                 the marker will no longer be inherited by child sessions",
            ),
            (
                "B2 answer explaining the interrupt shortcut",
                "Two shortcuts are worth remembering when you drive the CLI by hand.\n\
                 Press esc to interrupt",
            ),
            (
                "B3 answer explaining the shortcut hint",
                "The footer of the composer is a hint bar.\n\
                 It shows ? for shortcuts",
            ),
            (
                "B5 English answer about MCP authentication",
                "Your integration is not wired up yet.\n\
                 The MCP server needs authentication before any of its tools become callable",
            ),
            (
                "A9 answer about permission modes",
                "There are three permission modes you can cycle through.\n\
                 The default is manual mode on start-up",
            ),
            (
                "A10 answer about the paste behaviour",
                "Long pastes are collapsed in the composer.\n\
                 You can paste again to expand",
            ),
            (
                "B6 Japanese answer",
                "設定を確認しました。\n\
                 Transcript saving is off のままなので、ログは保存されません",
            ),
            (
                "B7 Korean answer",
                "설정을 확인했습니다.\n\
                 Transcript saving is off 상태라 기록이 남지 않습니다",
            ),
            (
                "cost question answered in English",
                "Here is the breakdown you asked for.\n\
                 Total cost (USD) for the month came to $12.30 across every workspace",
            ),
            (
                "update question answered in English",
                "Upgrading is straightforward.\n\
                 A new version available message means you should run npm i -g to refresh it",
            ),
        ];
        for (label, text) in cases {
            let v = strip_cli_noise(text);
            assert_eq!(v.text, *text, "{label}: content was modified");
            assert!(v.removed.is_empty(), "{label}: removed {:?}", v.removed);
        }
    }

    /// B9 — chrome-looking lines inside a fenced code block are content.
    #[test]
    fn code_fence_content_is_never_removed() {
        let text = "這是你要的終端輸出：\n\
             ```text\n\
             ⚠ Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker\n\
             ⏵⏵ auto mode on (shift+tab to cycle)\n\
             esctointerrupt\n\
             ```\n\
             以上就是完整畫面。";
        let v = strip_cli_noise(text);
        assert_eq!(v.text, text);
        assert!(v.removed.is_empty());
        assert!(!v.kept_suspicious.is_empty(), "should still be reported as suspicious");
    }

    /// The multi-line version of the old single-line test: real answers that
    /// merely discuss these topics survive, including when the mention is the
    /// final line (the strict patterns cannot be removed on position alone).
    #[test]
    fn keeps_user_content_that_merely_talks_about_these_topics() {
        let cases: &[&str] = &[
            "你問的是中斷方式。\n\
             你可以按 Esc 中斷目前的任務，或用 ? 查看快捷鍵。\n\
             兩者都不會結束整個工作階段。",
            "先說結論：兩個名詞不一樣。\n\
             Transcript 這個詞在會議紀錄的語境下指逐字稿，不是設定項目。\n\
             設定項目那個叫 session persistence。",
            "這是我剛才查到的做法。\n\
             設定 CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 就能保留逐字稿。\n\
             需要我幫你寫進啟動腳本嗎？",
            "The integration is half-configured.\n\
             The MCP server needs authentication before the tools become available.\n\
             Here is how the handshake works and what to configure on your side",
            "這個快捷鍵我幫你查過了。\n\
             ctrl+g to edit in Vim 在 Claude Code 裡可以開啟編輯器。\n\
             按 Esc 可以退回來。",
        ];
        for c in cases {
            let v = strip_cli_noise(c);
            assert_eq!(v.text, *c, "must not modify: {c}");
            assert!(v.removed.is_empty(), "must not remove from: {c}");
        }
    }

    /// Rule (f): a strict pattern in trailing position with no glyph and no
    /// glued render is kept, while the same phrase glued IS removed.
    #[test]
    fn strict_patterns_need_glyph_or_glued_not_position() {
        let prose = "說明如下。\nIt shows ? for shortcuts";
        let v = strip_cli_noise(prose);
        assert_eq!(v.text, prose);
        assert_eq!(v.kept_suspicious, vec!["interrupt_hint"]);

        let glued = "說明如下。\n?forshortcuts";
        let v = strip_cli_noise(glued);
        assert_eq!(v.text, "說明如下。");
        assert_eq!(v.removed, vec!["interrupt_hint"]);
    }

    /// Every pattern whose wording a genuine support answer can reuse must be
    /// unremovable on trailing position alone. These are the exact lines that
    /// rule (c) would have eaten before the strict flags were set — each one is
    /// a plausible last line of a real reply.
    #[test]
    fn prose_in_trailing_position_is_never_removed() {
        let tails: &[&str] = &[
            "Set CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 if you want transcripts kept",
            "Transcript saving is off for headless runs",
            "Use bypass permissions only when you trust the repository",
            "Run /init to create a CLAUDE.md for this repo",
            "A credit balance too low error means you have to top up first",
            "Run /compact to shrink the context window",
            "To exit press Ctrl+C twice",
            "Do you trust the files in this folder is the prompt you will see",
            "It shows ? for shortcuts",
            "The MCP server needs authentication first",
            "You can paste again to expand",
        ];
        for tail in tails {
            let text = format!("先講結論。\n中間還有一段說明。\n{tail}");
            let v = strip_cli_noise(&text);
            assert_eq!(v.text, text, "trailing prose was eaten: {tail}");
            assert!(v.removed.is_empty(), "{tail}: removed {:?}", v.removed);
        }
    }

    /// Rule (c) refuses lines that read like a sentence, even at the tail.
    #[test]
    fn trailing_rule_skips_sentence_like_lines() {
        let text = "第一段。\nYou can paste again to expand.";
        let v = strip_cli_noise(text);
        assert_eq!(v.text, text);
    }

    /// Veto (e): kana / Hangul lines are protected exactly like Han lines.
    #[test]
    fn cjk_kana_and_hangul_lines_are_kept() {
        for line in [
            "提醒：transcript saving is off 代表逐字稿不會被保存喔",
            "ヒント: transcript saving is off の状態です",
            "참고: transcript saving is off 상태입니다",
        ] {
            let v = strip_cli_noise(line);
            assert_eq!(v.text, line);
            assert_eq!(v.kept_suspicious, vec!["transcript_persistence"]);
        }
    }

    /// **Known limitation (review finding R1).** When the agent quotes the
    /// chrome line *verbatim* — same glyph, same spacing, on its own line —
    /// that quoted line is removed even though it is legitimate content.
    ///
    /// This is accepted, not overlooked. At the granularity this filter works
    /// at (one output line, no other inputs) the quoted line is **byte-identical
    /// to real chrome**, so no local rule can separate the two: any test that
    /// spared the quote would equally spare the leak we are here to stop.
    /// Rescuing it would require correlating the outbound text against the
    /// inbound user message (did the user ask about this string?), which is a
    /// design change — a new input to the filter — not a tuning knob, and it
    /// would hand an injection lever to anyone who can write the inbound text.
    ///
    /// Blast radius is deliberately small: only the quoted line goes, the
    /// answer around it is untouched, and the standard way to quote terminal
    /// output — a fenced code block — is already immune (veto d).
    #[test]
    fn known_limitation_verbatim_quote_of_real_chrome_is_removed() {
        let input = "你問的那句系統訊息是這樣寫的：\n\
             ⚠ Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker\n\
             意思是逐字稿不會被保存，不影響對話本身。";
        let v = strip_cli_noise(input);
        // Current behaviour, asserted so a future change is a deliberate one:
        // the quoted line is dropped…
        assert_eq!(
            v.text,
            "你問的那句系統訊息是這樣寫的：\n意思是逐字稿不會被保存，不影響對話本身。"
        );
        assert_eq!(v.removed, vec!["transcript_persistence"]);

        // …but the answer body survives, and the fenced-quote form — how
        // terminal output is normally pasted — keeps everything.
        let fenced = "你問的那句系統訊息是這樣寫的：\n\
             ```text\n\
             ⚠ Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker\n\
             ```\n\
             意思是逐字稿不會被保存，不影響對話本身。";
        let v = strip_cli_noise(fenced);
        assert_eq!(v.text, fenced, "fenced quote is the supported workaround");
        assert!(v.removed.is_empty());
    }

    #[test]
    fn passthrough_when_no_tripwire_matches() {
        let text = "這是一則完全正常的回覆，含 English words and 數字 123。";
        let v = strip_cli_noise(text);
        assert_eq!(v, NoiseVerdict::unchanged(text));
    }
}
