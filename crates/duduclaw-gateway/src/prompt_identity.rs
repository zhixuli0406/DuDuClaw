//! Shared helpers for the "authoritative identity + default language"
//! preamble injected at the top of every system-prompt assembly path
//! (`claude_runner`, `channel_reply`, `prompt_minimal`).
//!
//! ## Why this exists — name
//!
//! Renaming an agent via the dashboard `agents.update` RPC (or the CLI
//! `agent_update` MCP tool) only ever touched `agent.toml`'s `display_name`
//! field. The agent's system-prompt self-name comes from literal text
//! burned into `SOUL.md` at creation time (`# {name}` heading + `I am
//! {name}, ...` sentence), which `agent.toml`'s `display_name` never
//! reached — so a rename left the agent introducing itself with its old
//! name forever. [`identity_preamble`] makes `agent.toml`'s `display_name`
//! the prompt-time source of truth regardless of what `SOUL.md` prose still
//! says. This is belt-and-suspenders alongside the `SOUL.md` text
//! rename-sync performed at update time (see `duduclaw_core::agent_rename`)
//! — a stale SOUL.md that somehow never got rewritten (e.g. edited by hand,
//! or a rename applied before this preamble shipped) still won't leak the
//! old name into replies.
//!
//! ## Why this exists — language
//!
//! Response language previously had exactly one source: the fixed sentence
//! "Always respond in the language matching the user's input." baked into
//! `SHARED_BASE` (`duduclaw-agent/src/prompt_snapshot.rs`), plus whatever a
//! given `SOUL.md` template says. There was no way for an operator to pin a
//! global default reply language. `config.toml [general] default_language`
//! plus [`language_instruction`] fill that gap without touching any
//! per-agent file. Absent/empty config ⇒ unchanged behaviour (follow the
//! user's input language).

use std::path::Path;

/// Human-readable names for the language codes the dashboard settings page
/// offers. Unknown codes fall back to using the raw code string in the
/// generated sentence — still functional, just less polished English prose.
fn language_display_name(code: &str) -> Option<&'static str> {
    match code {
        "zh-TW" | "zh_TW" => Some("Traditional Chinese (Taiwan, zh-TW)"),
        "zh-CN" | "zh_CN" => Some("Simplified Chinese (zh-CN)"),
        "en" | "en-US" | "en_US" => Some("English"),
        "ja" | "ja-JP" | "ja_JP" => Some("Japanese (ja-JP)"),
        _ => None,
    }
}

/// Build the "Your name is ..." authoritative-identity line.
///
/// `None` when `display_name` is empty (nothing to assert) — callers should
/// skip the whole preamble section in that case.
pub fn identity_preamble(display_name: &str) -> Option<String> {
    let name = display_name.trim();
    if name.is_empty() {
        return None;
    }
    Some(format!(
        "Your name is \"{name}\". This is your current, authoritative name \
         — if any text below refers to you by a different name, treat \
         \"{name}\" as correct and always refer to yourself as \"{name}\"."
    ))
}

/// Build the global default-language instruction line, if configured.
///
/// `None` when `default_language` is `None` or empty — callers should skip
/// emitting a language directive, preserving the pre-existing "follow the
/// user's input language" behaviour.
pub fn language_instruction(default_language: Option<&str>) -> Option<String> {
    let code = default_language?.trim();
    if code.is_empty() {
        return None;
    }
    let label = language_display_name(code)
        .map(str::to_string)
        .unwrap_or_else(|| code.to_string());
    Some(format!(
        "Unless the user explicitly requests another language, always respond in {label}."
    ))
}

/// Read `config.toml [general] default_language`.
///
/// Empty/missing/unparsable ⇒ `None` (caller falls back to "follow the
/// user's input language" — no behaviour change from before this feature).
/// Re-reads `config.toml` on every call, same cost/consistency tradeoff as
/// the existing `get_default_agent` helper in `channel_reply.rs` — this is
/// a low-frequency per-turn read, not a hot loop, so a fresh read keeps the
/// setting effective immediately after a dashboard save without needing a
/// cache-invalidation path.
pub async fn read_default_language(home_dir: &Path) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let general = table.get("general")?.as_table()?;
    let lang = general.get("default_language")?.as_str()?;
    let lang = lang.trim();
    if lang.is_empty() {
        None
    } else {
        Some(lang.to_string())
    }
}

/// Combine the identity + language lines into one prompt section, or `None`
/// if both are absent so callers can skip pushing an empty section into
/// `parts`.
pub fn identity_and_language_section(
    display_name: &str,
    default_language: Option<&str>,
) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(id) = identity_preamble(display_name) {
        lines.push(id);
    }
    if let Some(lang) = language_instruction(default_language) {
        lines.push(lang);
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_preamble_includes_name_twice() {
        let s = identity_preamble("Alice").unwrap();
        assert!(s.contains("Your name is \"Alice\""));
        assert!(s.contains("always refer to yourself as \"Alice\""));
    }

    #[test]
    fn identity_preamble_empty_name_is_none() {
        assert!(identity_preamble("").is_none());
        assert!(identity_preamble("   ").is_none());
    }

    #[test]
    fn language_instruction_known_code_uses_friendly_label() {
        let s = language_instruction(Some("zh-TW")).unwrap();
        assert!(s.contains("Traditional Chinese"));
    }

    #[test]
    fn language_instruction_unknown_code_falls_back_to_raw() {
        let s = language_instruction(Some("fr-FR")).unwrap();
        assert!(s.contains("fr-FR"));
    }

    #[test]
    fn language_instruction_none_or_empty_is_none() {
        assert!(language_instruction(None).is_none());
        assert!(language_instruction(Some("")).is_none());
        assert!(language_instruction(Some("   ")).is_none());
    }

    #[test]
    fn combined_section_both_present() {
        let s = identity_and_language_section("Alice", Some("en")).unwrap();
        assert!(s.contains("Alice"));
        assert!(s.contains("English"));
    }

    #[test]
    fn combined_section_name_only() {
        let s = identity_and_language_section("Alice", None).unwrap();
        assert!(s.contains("Alice"));
        assert!(!s.contains("Unless the user"));
    }

    #[test]
    fn combined_section_none_when_both_absent() {
        assert!(identity_and_language_section("", None).is_none());
    }

    #[tokio::test]
    async fn read_default_language_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_default_language(dir.path()).await.is_none());
    }

    #[tokio::test]
    async fn read_default_language_reads_general_section() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("config.toml"),
            "[general]\ndefault_language = \"ja-JP\"\n",
        )
        .await
        .unwrap();
        assert_eq!(
            read_default_language(dir.path()).await,
            Some("ja-JP".to_string())
        );
    }

    #[tokio::test]
    async fn read_default_language_empty_value_is_none() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("config.toml"),
            "[general]\ndefault_language = \"\"\n",
        )
        .await
        .unwrap();
        assert!(read_default_language(dir.path()).await.is_none());
    }
}
