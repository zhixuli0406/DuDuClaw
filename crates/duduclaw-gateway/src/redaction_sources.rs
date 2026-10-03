//! `[redaction.sources]` wiring for the sources other than tool results.
//!
//! The redaction crate decides per [`Source`] whether to redact (`on` /
//! `off` / `selective` / `inherit`). Until v1.68.0 only `ToolResult` was
//! ever constructed on a live path, so the dashboard's 資料來源保護 rows for
//! user messages, the system prompt and cron context did nothing. This
//! module is the one place those call sites go through:
//!
//! - user messages → [`Source::UserChannelInput`] (channel reply, before the
//!   prompt is built);
//! - the assembled system prompt → [`Source::SystemPrompt`] (channel reply);
//! - a cron task's trigger context → [`Source::CronContext`] (cron scheduler);
//! - a delegated agent's reply written into the delegating agent's session
//!   history → [`Source::SubAgentReply`] (dispatcher,
//!   `append_subagent_reply_to_parent_session`). That is the one path where
//!   one agent's output becomes another agent's model context verbatim:
//!   `send_to_agent`, `spawn_agent` and `spawn_ephemeral` replies all return
//!   through it. The pass runs under the agent that owns the session (the
//!   direct parent, or the chain root on a relayed append) and that session
//!   id, so the owner's channel reply restores the tokens like any other.
//!   The copy delivered to the user's channel is the user's own view and is
//!   not redacted. Default mode `inherit` = passthrough (the child already
//!   ran under the same `[redaction]` config); `on` applies the parent's
//!   rules. Replies an agent fetches itself with `check_responses` arrive as
//!   a tool result and follow `tool_results`, not this source.
//!
//! Fail closed: a redaction error means the text must not reach the model;
//! callers stop the turn.

use std::sync::{Arc, RwLock};

use duduclaw_redaction::{RedactionManager, Source};

static CURRENT: RwLock<Option<Arc<RedactionManager>>> = RwLock::new(None);

/// Record the live manager (set by `swap_redaction_manager`, at boot and on
/// every `redaction.update`), for call sites without a `ReplyContext`.
pub fn set_current(manager: Option<Arc<RedactionManager>>) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = manager;
}

/// The live manager, `None` when redaction is off.
pub fn current() -> Option<Arc<RedactionManager>> {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Redact `text` from `source` for `(agent_id, session_id)`. No manager ⇒
/// the text unchanged. `Err` ⇒ the caller must not send the text onward.
pub fn redact_text(
    manager: Option<&Arc<RedactionManager>>,
    agent_id: &str,
    session_id: Option<&str>,
    text: &str,
    source: &Source,
) -> Result<String, String> {
    let Some(manager) = manager else {
        return Ok(text.to_string());
    };
    if text.is_empty() {
        return Ok(String::new());
    }
    let pipeline = manager
        .pipeline(agent_id, session_id.map(str::to_string))
        .map_err(|e| format!("redaction pipeline unavailable: {e}"))?;
    pipeline
        .redact(text, source)
        .map(|out| out.redacted_text)
        .map_err(|e| format!("redaction failed ({}): {e}", source.category()))
}

/// The body placed into a parent's session instead of a sub-agent reply whose
/// `sub_agent` redaction pass failed (fail closed: the raw text is dropped).
pub const SUB_AGENT_REPLY_WITHHELD: &str =
    "[子代理回覆未載入：去識別化處理失敗，原文已依安全設定擋下]";

/// Redact a delegated agent's reply before it enters the session owned by
/// `owner_agent` (`[redaction.sources] sub_agent`). The pass runs under the
/// owner's agent id and `session_id` so restore on the owner's way out finds
/// the mapping; the audit row's source detail names `responder_agent`.
/// `Err` ⇒ the caller must not place the raw reply into that context.
///
/// Only mode `on` builds a pipeline: every other mode passes the text
/// through in the pipeline too, and checking first keeps the default
/// (`inherit`) byte-identical to the pre-wiring path even when the owner's
/// vault key cannot be loaded.
pub fn redact_sub_agent_reply(
    manager: Option<&Arc<RedactionManager>>,
    owner_agent: &str,
    session_id: &str,
    responder_agent: &str,
    text: &str,
) -> Result<String, String> {
    let Some(m) = manager else {
        return Ok(text.to_string());
    };
    if m.source_policy().sub_agent.mode != duduclaw_redaction::config::SourceMode::On {
        return Ok(text.to_string());
    }
    redact_text(
        manager,
        owner_agent,
        Some(session_id),
        text,
        &Source::SubAgentReply {
            agent_id: responder_agent.to_string(),
        },
    )
}

/// Restore tokens in text bound for the end user's channel (owner caller).
/// Errors return the text unchanged (tokens stay opaque, which is safe).
pub fn restore_for_user(
    manager: Option<&Arc<RedactionManager>>,
    agent_id: &str,
    session_id: Option<&str>,
    text: String,
) -> String {
    let Some(manager) = manager else { return text };
    if !text.contains(duduclaw_redaction::token::TOKEN_PREFIX) {
        return text;
    }
    let Ok(pipeline) = manager.pipeline(agent_id, session_id.map(str::to_string)) else {
        return text;
    };
    pipeline
        .restore(
            &text,
            &duduclaw_redaction::Caller::owner(agent_id),
            duduclaw_redaction::RestoreTarget::UserChannel,
        )
        .unwrap_or(text)
}

/// The GC settings a manager's config asks for (`purge_after_expire_days`
/// from `[redaction]`; intervals stay at the crate defaults).
pub fn gc_config_for(manager: &RedactionManager) -> duduclaw_redaction::GcConfig {
    duduclaw_redaction::GcConfig {
        purge_after_expire_days: manager.purge_after_expire_days(),
        ..duduclaw_redaction::GcConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(home: &std::path::Path, sources: &str, extra: &str) -> Arc<RedactionManager> {
        let src = format!(
            "enabled = true\nprofiles = [\"general\"]\n{extra}\n[sources]\n{sources}\n"
        );
        let cfg: duduclaw_redaction::RedactionConfig = toml::from_str(&src).unwrap();
        Arc::new(
            RedactionManager::open(cfg, duduclaw_redaction::ManagerPaths::under_home(home))
                .unwrap(),
        )
    }

    /// Like [`manager`] but with extra tables after `[sources]`.
    fn manager_with_rules(
        home: &std::path::Path,
        sources: &str,
        tables: &str,
    ) -> Arc<RedactionManager> {
        let src = format!(
            "enabled = true\nprofiles = [\"general\"]\n[sources]\n{sources}\n{tables}"
        );
        let cfg: duduclaw_redaction::RedactionConfig = toml::from_str(&src).unwrap();
        Arc::new(
            RedactionManager::open(cfg, duduclaw_redaction::ManagerPaths::under_home(home))
                .unwrap(),
        )
    }

    const EMAIL_TEXT: &str = "請寄給 alice@example.com 謝謝";

    #[test]
    fn user_input_follows_its_toggle() {
        let tmp = tempfile::tempdir().unwrap();
        let user = Source::UserChannelInput {
            channel_id: "telegram".into(),
        };
        // Default `off` ⇒ unchanged.
        let m = manager(tmp.path(), "", "");
        let out = redact_text(Some(&m), "main", Some("telegram:1"), EMAIL_TEXT, &user).unwrap();
        assert_eq!(out, EMAIL_TEXT);
        // `on` ⇒ the e-mail becomes a token, and restores for the owner.
        let tmp = tempfile::tempdir().unwrap();
        let m = manager(tmp.path(), "user_input = \"on\"", "");
        let out = redact_text(Some(&m), "main", Some("telegram:1"), EMAIL_TEXT, &user).unwrap();
        assert!(!out.contains("alice@example.com"), "{out}");
        assert!(out.contains(duduclaw_redaction::token::TOKEN_PREFIX));
        let back = restore_for_user(Some(&m), "main", Some("telegram:1"), out);
        assert_eq!(back, EMAIL_TEXT);
    }

    #[test]
    fn cron_context_default_on_and_can_be_switched_off() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manager(tmp.path(), "", "");
        let out =
            redact_text(Some(&m), "main", Some("cron:1"), EMAIL_TEXT, &Source::CronContext).unwrap();
        assert!(!out.contains("alice@example.com"));
        let tmp = tempfile::tempdir().unwrap();
        let m = manager(tmp.path(), "cron_context = \"off\"", "");
        let out =
            redact_text(Some(&m), "main", Some("cron:1"), EMAIL_TEXT, &Source::CronContext).unwrap();
        assert_eq!(out, EMAIL_TEXT);
    }

    #[test]
    fn system_prompt_selective_skips_rules_not_marked_for_it() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manager(tmp.path(), "", "");
        let sp = Source::SystemPrompt {
            component: "channel_reply".into(),
        };
        // `general` rules are not `apply_to_system_prompt` ⇒ untouched.
        assert_eq!(
            redact_text(Some(&m), "main", None, EMAIL_TEXT, &sp).unwrap(),
            EMAIL_TEXT
        );
        // A rule marked `apply_to_system_prompt` fires under `selective`…
        let marked = "[rules.sp_email]\ntype = \"regex\"\npattern = '[a-z]+@example\\.com'\ncategory = \"EMAIL\"\npriority = 60\napply_to_system_prompt = true\n";
        let tmp = tempfile::tempdir().unwrap();
        let m = manager_with_rules(tmp.path(), "", marked);
        assert!(!redact_text(Some(&m), "main", None, EMAIL_TEXT, &sp)
            .unwrap()
            .contains("alice@example.com"));
        // …and not when the source is switched off.
        let tmp = tempfile::tempdir().unwrap();
        let m = manager_with_rules(tmp.path(), "system_prompt = \"off\"", marked);
        assert_eq!(
            redact_text(Some(&m), "main", None, EMAIL_TEXT, &sp).unwrap(),
            EMAIL_TEXT
        );
    }

    #[test]
    fn sub_agent_inherit_and_off_are_byte_identical() {
        // Default (`inherit`) and explicit `off` both pass the reply through.
        for sources in ["", "sub_agent = \"off\"", "sub_agent = \"inherit\""] {
            let tmp = tempfile::tempdir().unwrap();
            let m = manager(tmp.path(), sources, "");
            let out =
                redact_sub_agent_reply(Some(&m), "agnes", "telegram:1", "tl", EMAIL_TEXT).unwrap();
            assert_eq!(out.as_bytes(), EMAIL_TEXT.as_bytes(), "sources={sources:?}");
        }
        assert_eq!(
            redact_sub_agent_reply(None, "agnes", "telegram:1", "tl", EMAIL_TEXT).unwrap(),
            EMAIL_TEXT
        );
    }

    #[test]
    fn sub_agent_on_redacts_under_owner_and_restores_for_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manager(tmp.path(), "sub_agent = \"on\"", "");
        let out =
            redact_sub_agent_reply(Some(&m), "agnes", "telegram:1", "tl", EMAIL_TEXT).unwrap();
        assert!(!out.contains("alice@example.com"), "{out}");
        assert!(out.contains(duduclaw_redaction::token::TOKEN_PREFIX));
        // The owner (parent) restores it on its way out to the user...
        let back = restore_for_user(Some(&m), "agnes", Some("telegram:1"), out.clone());
        assert_eq!(back, EMAIL_TEXT);
        // ...and the audit row names the source and the responding agent.
        let audit = std::fs::read_to_string(tmp.path().join("redaction/audit.jsonl")).unwrap();
        assert!(audit.contains("sub_agent_reply"), "{audit}");
        assert!(audit.contains("\"tl\""), "{audit}");
    }

    #[test]
    fn sub_agent_on_fails_closed_when_the_pipeline_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manager(tmp.path(), "sub_agent = \"on\"", "");
        // A key file of the wrong length makes the owner's pipeline unbuildable.
        let keys = tmp.path().join("redaction/keys");
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::write(keys.join("agnes.key"), b"short").unwrap();
        let err = redact_sub_agent_reply(Some(&m), "agnes", "telegram:1", "tl", EMAIL_TEXT);
        assert!(err.is_err(), "{err:?}");
        // The same broken key does not affect the default mode.
        let tmp2 = tempfile::tempdir().unwrap();
        let m2 = manager(tmp2.path(), "", "");
        let keys2 = tmp2.path().join("redaction/keys");
        std::fs::create_dir_all(&keys2).unwrap();
        std::fs::write(keys2.join("agnes.key"), b"short").unwrap();
        assert_eq!(
            redact_sub_agent_reply(Some(&m2), "agnes", "telegram:1", "tl", EMAIL_TEXT).unwrap(),
            EMAIL_TEXT
        );
    }

    #[test]
    fn no_manager_is_passthrough_and_gc_reads_purge_days() {
        assert_eq!(
            redact_text(None, "main", None, EMAIL_TEXT, &Source::CronContext).unwrap(),
            EMAIL_TEXT
        );
        let tmp = tempfile::tempdir().unwrap();
        let m = manager(tmp.path(), "", "purge_after_expire_days = 7");
        assert_eq!(gc_config_for(&m).purge_after_expire_days, 7);
    }
}
