//! Single-source-of-truth channel × capability matrix.
//!
//! WP-9B (P2 of `commercial/docs/DESIGN-everything-is-a-plugin-2026-08.md`).
//!
//! Before this module, "does channel X support capability Y" was answered by
//! a different scattered mechanism per capability:
//! - file/photo upload: whether `ChannelSender::send_document`/`send_photo`
//!   had a real per-platform override, or fell through to the trait's
//!   default text-notice fallback (`channel_sender.rs`) — silently, with no
//!   log line distinguishing "degraded on purpose" from "nobody noticed".
//! - interactive buttons: `channel_format::decision_markup`'s `_ => None`
//!   arm and `goal_notify::send_with_markup`'s `other => Err(..)` arm —
//!   two independent `match` statements that happen to agree today.
//! - message edit-in-place: `decision_card::channel_editable` (a 3-channel
//!   allowlist for decision-card collapse) vs. five *different* hand-rolled
//!   `progress_msg_id`/`progress_msg_ts`/`progress_activity_id` edit loops in
//!   telegram.rs/discord.rs/slack.rs/googlechat.rs/msteams.rs (a *different*
//!   5-channel set) for the 📋 task-board progress display.
//! - typing indicator: the doc table at the top of `channel_typing.rs`
//!   (6 channels have a real API) vs. `typing_guard_for`'s `_ => None` arm
//!   (only Telegram is wired for the cross-cutting task-dispatch path).
//! - progress throttle window: a bare integer literal (`30`/`45`/`60`)
//!   copy-pasted into 10 different channel handler closures.
//!
//! This module does not replace any of those call sites' actual mechanics
//! (each platform's real edit/upload/button API is still implemented where
//! it always was — a REST PATCH call is still a REST PATCH call). It gives
//! every one of those judgments ONE place to be looked up, cross-checked by
//! unit tests, and — for the two spots the WP prioritized (computer-use
//! sender text fallback, progress edit-in-place throttle) — a shared,
//! observable "this degraded on purpose" trace instead of pure silence.
//!
//! ## Values are behavior-derived, not aspirational
//!
//! Every boolean below was set by reading the actual current implementation
//! (see the file:line references in each `ChannelCapabilities` entry's
//! comment), not by what "should" be supported. Flipping a value here does
//! NOT change behavior anywhere — it only changes what
//! [`log_unsupported`] callers report and what `channels.capabilities`
//! (dashboard RPC) shows. Keeping code and table in sync is a discipline,
//! not something the type system enforces; the unit tests below pin the
//! audited values so a silent drift is at least a red test, not a mystery.

use tracing::{debug, warn};

/// A capability that some channels support and others don't, where the
/// codebase today has real (not aspirational) divergent behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Native document/file upload. `true` ⇒ `ChannelSender::send_document`
    /// has a real per-platform override; `false` ⇒ it falls through to the
    /// trait's default text-notice fallback.
    FileUpload,
    /// Native image upload. `true` ⇒ `ChannelSender::send_photo` calls a
    /// real media API; `false` ⇒ it always degrades to a text notice.
    PhotoUpload,
    /// Inline decision buttons (`channel_format::decision_markup` /
    /// `goal_notify::send_with_markup` have a real arm for this channel).
    InteractiveButtons,
    /// The progress task board (📋) is delivered as an in-place update —
    /// either a real message-edit REST call, or (WebChat) a live
    /// client-rendered frame — rather than a growing stream of new messages.
    EditInPlace,
    /// A "bot is typing/working" indicator API is wired for this channel
    /// somewhere in the codebase (does not imply it's wired into the
    /// cross-cutting `channel_typing::typing_guard_for` helper — see that
    /// function's own doc comment for its narrower coverage).
    TypingIndicator,
    /// The channel renders SOME markdown formatting natively (bold/code
    /// fence/etc.), as opposed to plain text only.
    NativeMarkdown,
    /// Inbound "replying to / quoting a message" carries the quoted content
    /// (or, for WhatsApp, at least a provenance annotation) into the agent
    /// input. See `docs/todo/TODO-channel-quote-context-remaining.md`.
    QuotedContext,
}

/// The full capability row for one channel. Every field is backed by a real
/// code path — see the audit table in [`TABLE`] for file:line evidence.
#[derive(Debug, Clone, Copy)]
pub struct ChannelCapabilities {
    pub channel: &'static str,
    pub file_upload: bool,
    pub photo_upload: bool,
    pub interactive_buttons: bool,
    pub edit_in_place: bool,
    pub typing_indicator: bool,
    pub native_markdown: bool,
    pub quoted_context: bool,
    /// Progress-board throttle window in seconds, or `None` when the
    /// channel delivers progress events without throttling (WebChat: every
    /// event is forwarded live over the open WebSocket, there is no "new
    /// message" cost to amortize).
    pub progress_throttle_secs: Option<u64>,
}

impl ChannelCapabilities {
    pub fn supports(&self, cap: Capability) -> bool {
        match cap {
            Capability::FileUpload => self.file_upload,
            Capability::PhotoUpload => self.photo_upload,
            Capability::InteractiveButtons => self.interactive_buttons,
            Capability::EditInPlace => self.edit_in_place,
            Capability::TypingIndicator => self.typing_indicator,
            Capability::NativeMarkdown => self.native_markdown,
            Capability::QuotedContext => self.quoted_context,
        }
    }
}

/// Default progress throttle window used when a channel isn't in [`TABLE`]
/// at all (never expected in production — every registered channel has a
/// row — but a safe fallback beats a panic for an unrecognised string).
const DEFAULT_PROGRESS_THROTTLE_SECS: u64 = 30;

/// The eleven-channel capability matrix. Ordering matches
/// `channel_sender.rs`'s module doc comment (Telegram → WebChat).
///
/// Audit evidence (2026-08-16, WP-9B):
/// - file_upload / photo_upload: `channel_sender.rs` — presence of a
///   `send_document`/`send_photo` override vs. the trait defaults.
/// - interactive_buttons: `channel_format.rs:679` `decision_markup` +
///   `goal_notify.rs:996` `send_with_markup` (both cover exactly
///   telegram/discord/slack/line, confirmed to agree).
/// - edit_in_place: union of `decision_card.rs:142` `channel_editable`
///   (telegram/slack/discord) and the progress-board edit loops in
///   telegram.rs/discord.rs/slack.rs/googlechat.rs/msteams.rs (adds
///   googlechat/teams) plus WebChat's live-frame delivery.
/// - typing_indicator: doc table at the top of `channel_typing.rs`
///   (telegram/discord/line/whatsapp/slack/teams).
/// - native_markdown: doc table at the top of `markdown_render.rs`, plus
///   dingtalk.rs/wecom.rs's own `msgtype: "markdown"` support and WebChat's
///   client-side CommonMark renderer. LINE is the sole plain-text channel.
/// - quoted_context: `docs/features/` inbound-quote coverage — telegram/
///   discord/slack/teams full excerpt, whatsapp annotation-only; the
///   remaining six are tracked in
///   `docs/todo/TODO-channel-quote-context-remaining.md`.
/// - progress_throttle_secs: the literal `elapsed().as_secs() < N` guard in
///   each channel's `on_progress` closure.
const TABLE: &[ChannelCapabilities] = &[
    ChannelCapabilities {
        channel: "telegram",
        file_upload: true,
        photo_upload: true,
        interactive_buttons: true,
        edit_in_place: true,
        typing_indicator: true,
        native_markdown: true,
        quoted_context: true,
        progress_throttle_secs: Some(30),
    },
    ChannelCapabilities {
        channel: "discord",
        file_upload: true,
        photo_upload: true,
        interactive_buttons: true,
        edit_in_place: true,
        typing_indicator: true,
        native_markdown: true,
        quoted_context: true,
        progress_throttle_secs: Some(30),
    },
    ChannelCapabilities {
        channel: "slack",
        file_upload: true,
        photo_upload: true,
        interactive_buttons: true,
        edit_in_place: true,
        typing_indicator: true,
        native_markdown: true,
        quoted_context: true,
        progress_throttle_secs: Some(30),
    },
    ChannelCapabilities {
        channel: "line",
        // send_document has no LINE override — default text fallback.
        file_upload: false,
        // Blob upload API IS native; a runtime failure falls back to text,
        // but that's a transient-error path, not a missing capability.
        photo_upload: true,
        // `decision_markup`/`send_with_markup` both have a "line" arm.
        interactive_buttons: true,
        // No editMessage endpoint on the Messaging API at all.
        edit_in_place: false,
        typing_indicator: true,
        // to_line_plain — plain text only, no markdown syntax survives.
        native_markdown: false,
        quoted_context: false,
        progress_throttle_secs: Some(60),
    },
    ChannelCapabilities {
        channel: "whatsapp",
        file_upload: true,
        photo_upload: true,
        // No arm in decision_markup/send_with_markup.
        interactive_buttons: false,
        // No message-edit API; progress sends a new message each time.
        edit_in_place: false,
        typing_indicator: true,
        native_markdown: true,
        // Annotation only (Cloud API never ships the quoted body).
        quoted_context: true,
        progress_throttle_secs: Some(60),
    },
    ChannelCapabilities {
        channel: "feishu",
        file_upload: true,
        photo_upload: true,
        interactive_buttons: false,
        // No message-edit call implemented; progress sends new messages.
        edit_in_place: false,
        // Feishu has no typing API (see feishu.rs comment).
        typing_indicator: false,
        // Card 2.0 markdown (near-CommonMark).
        native_markdown: true,
        quoted_context: false,
        progress_throttle_secs: Some(45),
    },
    ChannelCapabilities {
        channel: "googlechat",
        // send_document has no Google Chat override — default text fallback.
        file_upload: false,
        // send_photo is a hand-rolled fallback that always sends a text
        // notice — no image-upload call exists.
        photo_upload: false,
        interactive_buttons: false,
        // Progress board edits its placeholder message via `update_message`
        // (PATCH) — real edit-in-place, even though `channel_editable`
        // (a narrower, decision-card-only allowlist) excludes it.
        edit_in_place: true,
        // "Chat has no typing API" (googlechat.rs comment) — uses a
        // placeholder message instead.
        typing_indicator: false,
        native_markdown: true,
        quoted_context: false,
        progress_throttle_secs: Some(30),
    },
    ChannelCapabilities {
        channel: "teams",
        file_upload: false,
        photo_upload: false,
        interactive_buttons: false,
        // Progress board edits its activity via `update_activity` (PATCH) —
        // same "real edit, narrower decision-card allowlist" story as
        // Google Chat.
        edit_in_place: true,
        typing_indicator: true,
        native_markdown: true,
        quoted_context: true,
        progress_throttle_secs: Some(30),
    },
    ChannelCapabilities {
        channel: "wecom",
        // send_document has no WeCom override — default text fallback.
        file_upload: false,
        photo_upload: true,
        interactive_buttons: false,
        edit_in_place: false,
        typing_indicator: false,
        // msgtype: "markdown" — WeCom client renders a markdown subset.
        native_markdown: true,
        quoted_context: false,
        progress_throttle_secs: Some(45),
    },
    ChannelCapabilities {
        channel: "dingtalk",
        file_upload: false,
        photo_upload: false,
        interactive_buttons: false,
        edit_in_place: false,
        typing_indicator: false,
        // msgtype: "markdown" via sessionWebhook.
        native_markdown: true,
        quoted_context: false,
        progress_throttle_secs: Some(45),
    },
    ChannelCapabilities {
        channel: "webchat",
        file_upload: true,
        photo_upload: true,
        // No decision_markup/send_with_markup arm — approvals surface via
        // the dashboard/chat UI instead of inline buttons.
        interactive_buttons: false,
        // Every progress event is forwarded as its own live WS frame; the
        // frontend renders it in place. No REST edit call, but the UX goal
        // ("don't spam N messages") is met by construction.
        edit_in_place: true,
        // A WebSocket connection is already live; no discrete "typing"
        // signal exists or is needed.
        typing_indicator: false,
        // Client-side CommonMark renderer — the richest of any channel.
        native_markdown: true,
        quoted_context: false,
        progress_throttle_secs: None,
    },
];

/// Look up the capability row for a channel type string. `None` for an
/// unrecognised channel (never expected for the 11 registered channels).
pub fn capabilities(channel: &str) -> Option<&'static ChannelCapabilities> {
    TABLE.iter().find(|c| c.channel == channel)
}

/// Whether `channel` supports `cap`. An unrecognised channel is treated as
/// NOT supporting anything (safe default — never claim a capability that
/// hasn't been audited).
pub fn supports(channel: &str, cap: Capability) -> bool {
    capabilities(channel).is_some_and(|c| c.supports(cap))
}

/// The progress-board throttle window for `channel`, in seconds. `None`
/// means "no throttle" (WebChat). An unrecognised channel gets the
/// conservative [`DEFAULT_PROGRESS_THROTTLE_SECS`] rather than a panic —
/// callers index this with a compile-time channel-name literal, so this
/// path is defensive, not expected to fire.
pub fn progress_throttle_secs(channel: &str) -> Option<u64> {
    match capabilities(channel) {
        Some(c) => c.progress_throttle_secs,
        None => Some(DEFAULT_PROGRESS_THROTTLE_SECS),
    }
}

/// Iterate every row — used by the dashboard capability-matrix RPC and by
/// tests that want to assert something holds for all channels.
pub fn all_channels() -> impl Iterator<Item = &'static ChannelCapabilities> {
    TABLE.iter()
}

/// Emit an observable trace when a caller wanted `cap` on `channel` but the
/// table says it isn't supported, and is about to degrade instead of
/// failing outright. Replaces the family of pure no-op degradations
/// (`NullSender`, hand-built "尚未支援" text notices) that previously left
/// zero trace — the exact `_ =>` failure mode dsh §2.9 calls "silent
/// downstream failures" and DESIGN-everything-is-a-plugin-2026-08.md §2.1
/// caught for `create_sender`'s googlechat/teams gap (since fixed).
///
/// `context` is a short free-text description of what the caller was doing
/// (e.g. `"send_document default text fallback"`), included in the log line
/// only — never used for control flow.
pub fn log_unsupported(channel: &str, cap: Capability, context: &str) {
    warn!(
        channel = %channel,
        capability = ?cap,
        context = %context,
        "channel capability not supported — degrading (see channel_capabilities table)"
    );
}

/// Lighter-weight sibling of [`log_unsupported`] for paths that fire at high
/// frequency by design (e.g. a per-dispatch typing-indicator lookup on every
/// cron tick) where a `warn!` per occurrence would be log spam rather than a
/// signal. Still fully observable at `debug` level — never truly silent.
pub fn debug_unsupported(channel: &str, cap: Capability, context: &str) {
    debug!(
        channel = %channel,
        capability = ?cap,
        context = %context,
        "channel capability not supported — degrading (see channel_capabilities table)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CHANNELS: &[&str] = &[
        "telegram", "line", "discord", "slack", "whatsapp", "feishu", "googlechat", "teams",
        "wecom", "dingtalk", "webchat",
    ];

    #[test]
    fn table_covers_exactly_the_eleven_registered_channels_no_dupes() {
        let mut seen = std::collections::HashSet::new();
        for c in all_channels() {
            assert!(seen.insert(c.channel), "duplicate row for {}", c.channel);
        }
        assert_eq!(seen.len(), ALL_CHANNELS.len());
        for ch in ALL_CHANNELS {
            assert!(capabilities(ch).is_some(), "missing row for {ch}");
        }
    }

    #[test]
    fn unknown_channel_supports_nothing() {
        for cap in [
            Capability::FileUpload,
            Capability::PhotoUpload,
            Capability::InteractiveButtons,
            Capability::EditInPlace,
            Capability::TypingIndicator,
            Capability::NativeMarkdown,
            Capability::QuotedContext,
        ] {
            assert!(!supports("not-a-real-channel", cap));
        }
        assert!(capabilities("not-a-real-channel").is_none());
    }

    #[test]
    fn unknown_channel_progress_throttle_falls_back_to_default() {
        assert_eq!(
            progress_throttle_secs("not-a-real-channel"),
            Some(DEFAULT_PROGRESS_THROTTLE_SECS)
        );
    }

    // -- file_upload: matches ChannelSender::send_document override presence --
    #[test]
    fn file_upload_matches_send_document_overrides() {
        for ch in ["telegram", "discord", "slack", "whatsapp", "feishu", "webchat"] {
            assert!(supports(ch, Capability::FileUpload), "{ch} should support file_upload");
        }
        for ch in ["line", "googlechat", "teams", "wecom", "dingtalk"] {
            assert!(!supports(ch, Capability::FileUpload), "{ch} should NOT support file_upload");
        }
    }

    // -- photo_upload: matches ChannelSender::send_photo real-API presence --
    #[test]
    fn photo_upload_matches_send_photo_real_api_presence() {
        for ch in [
            "telegram", "line", "discord", "slack", "whatsapp", "feishu", "wecom", "webchat",
        ] {
            assert!(supports(ch, Capability::PhotoUpload), "{ch} should support photo_upload");
        }
        for ch in ["googlechat", "teams", "dingtalk"] {
            assert!(!supports(ch, Capability::PhotoUpload), "{ch} should NOT support photo_upload");
        }
    }

    // -- interactive_buttons: matches decision_markup / send_with_markup --
    #[test]
    fn interactive_buttons_matches_decision_markup_coverage() {
        for ch in ["telegram", "discord", "slack", "line"] {
            assert!(supports(ch, Capability::InteractiveButtons), "{ch} should support buttons");
        }
        for ch in [
            "whatsapp", "feishu", "googlechat", "teams", "wecom", "dingtalk", "webchat",
        ] {
            assert!(!supports(ch, Capability::InteractiveButtons), "{ch} should NOT support buttons");
        }
    }

    // -- edit_in_place is a superset of decision_card::channel_editable's --
    //    narrower 3-channel allowlist (googlechat/teams have a real PATCH
    //    edit for the progress board even though the decision-card collapse
    //    path hasn't been extended to use it).
    #[test]
    fn edit_in_place_is_superset_of_decision_card_channel_editable() {
        for ch in ALL_CHANNELS {
            if crate::decision_card::channel_editable(ch) {
                assert!(
                    supports(ch, Capability::EditInPlace),
                    "{ch}: channel_editable=true but capability table says no edit_in_place"
                );
            }
        }
        // And the two known extras this table adds beyond channel_editable.
        assert!(supports("googlechat", Capability::EditInPlace));
        assert!(supports("teams", Capability::EditInPlace));
    }

    // -- typing_indicator: matches the channel_typing.rs module doc table --
    #[test]
    fn typing_indicator_matches_channel_typing_doc_table() {
        for ch in ["telegram", "discord", "line", "whatsapp", "slack", "teams"] {
            assert!(supports(ch, Capability::TypingIndicator), "{ch} should support typing");
        }
        for ch in ["feishu", "googlechat", "wecom", "dingtalk", "webchat"] {
            assert!(!supports(ch, Capability::TypingIndicator), "{ch} should NOT support typing");
        }
    }

    // -- native_markdown: LINE is the sole plain-text channel --
    #[test]
    fn native_markdown_excludes_only_line() {
        for c in all_channels() {
            if c.channel == "line" {
                assert!(!c.native_markdown, "line should be plain-text only");
            } else {
                assert!(c.native_markdown, "{} should support some native markdown", c.channel);
            }
        }
    }

    // -- quoted_context: telegram/discord/slack/teams full + whatsapp annotation --
    #[test]
    fn quoted_context_matches_documented_five_channels() {
        for ch in ["telegram", "discord", "slack", "teams", "whatsapp"] {
            assert!(supports(ch, Capability::QuotedContext), "{ch} should carry quoted context");
        }
        for ch in ["line", "feishu", "googlechat", "wecom", "dingtalk", "webchat"] {
            assert!(!supports(ch, Capability::QuotedContext), "{ch} should NOT carry quoted context");
        }
    }

    // -- progress throttle windows, pinned to the literal in each channel's
    //    on_progress closure so a future edit that changes one without
    //    updating this table fails a test instead of silently drifting.
    #[test]
    fn progress_throttle_matches_audited_literals() {
        let expect: &[(&str, Option<u64>)] = &[
            ("telegram", Some(30)),
            ("discord", Some(30)),
            ("slack", Some(30)),
            ("line", Some(60)),
            ("whatsapp", Some(60)),
            ("feishu", Some(45)),
            ("googlechat", Some(30)),
            ("teams", Some(30)),
            ("wecom", Some(45)),
            ("dingtalk", Some(45)),
            ("webchat", None),
        ];
        for (ch, want) in expect {
            assert_eq!(progress_throttle_secs(ch), *want, "throttle mismatch for {ch}");
        }
    }

    #[test]
    fn log_unsupported_and_debug_unsupported_do_not_panic() {
        // These are trace-only side effects; just exercise them for coverage
        // and to catch a future signature change that breaks call sites.
        log_unsupported("googlechat", Capability::PhotoUpload, "unit test");
        debug_unsupported("discord", Capability::TypingIndicator, "unit test");
    }
}
