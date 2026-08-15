//! Agent-visible preset line — WP-6F (agent presets P1), design §3.2 trace ③.
//!
//! # Why this exists
//!
//! `DESIGN-agent-presets-2026-08.md` §3.2 calls this "the soul of the whole
//! feature": a preset switch can silently change an agent's model, tools, and
//! evolution posture, but if the agent itself never sees that its job
//! composition changed, it keeps reasoning from the old SOUL-derived
//! self-image and blames itself for failures a capability change actually
//! caused (the same failure shape as the LWM D2/D3 incidents). The fix
//! pattern is already proven by `working_state.rs`: inject a durable,
//! machine-authoritative section into the dynamic prompt tail. This module
//! is that same pipeline, one section earlier.
//!
//! Reads `duduclaw_core::preset::resolve_for_agent` directly (the same
//! choke point `duduclaw-agent::registry::load_agent` uses) rather than
//! trusting any cached `AgentConfig` — the dynamic tail must reflect the
//! binding as it is *right now*, not as it was at the last registry scan.

use std::path::Path;

use duduclaw_core::preset::{PresetResolution, agent_home_dir, load_bindings};

/// Build the injectable preset-identity section, or `None` when the agent
/// has no binding (an unbound agent produces zero prompt noise — same
/// "empty ⇒ no section" discipline as `working_state`/`recent_actions`).
///
/// Placed BEFORE the working-state section in both dynamic-tail call sites
/// (design §3.2: "preset 行接在它前面即可") — standing job-composition
/// context first, then standing operational state, then action evidence.
pub fn build_preset_section(home_dir: &Path, agent_id: &str) -> Option<String> {
    let agent_dir = home_dir.join("agents").join(agent_id);
    let Some(home) = agent_home_dir(&agent_dir) else {
        return None;
    };
    if !load_bindings(&home).contains(agent_id) {
        return None; // Never bound — nothing to say.
    }

    let raw_table: toml::value::Table = std::fs::read_to_string(agent_dir.join("agent.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .unwrap_or_default();
    let (_, resolution) =
        duduclaw_core::preset::resolve_for_agent(&home, agent_id, &raw_table);

    let body = match resolution {
        PresetResolution::Unbound => return None,
        PresetResolution::Applied { preset_id, version, label, changed_fields, .. } => {
            let label = if label.trim().is_empty() { preset_id.clone() } else { label };
            let overrides = if changed_fields.is_empty() {
                String::new()
            } else {
                format!("；本機已覆寫：{}", changed_fields.join(", "))
            };
            format!("你目前套用職務組合「{label}」（{preset_id} v{version}）{overrides}。")
        }
        PresetResolution::Unresolved { preset_id, version, reason } => format!(
            "⚠️ 你原本綁定的職務組合「{preset_id}」（v{version}）目前無法套用（{reason}），\
             現在只用你自己 agent.toml 裡明寫的設定運作，工具與能力範圍可能比預期窄。\
             這不是你的錯——請回報管理者確認職務組合是否需要重新綁定。"
        ),
    };

    Some(format!(
        "## 目前職務組合（preset · 唯讀，由管理者以 `duduclaw preset bind` 設定）\n{body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_agent(home: &Path, id: &str, extra: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!(
                "[agent]\nname = \"{id}\"\ndisplay_name = \"{id}\"\nrole = \"worker\"\n\
                 status = \"active\"\ntrigger = \"@{id}\"\nreports_to = \"\"\nicon = \"x\"\n{extra}"
            ),
        )
        .unwrap();
    }

    fn write_preset(home: &Path, id: &str, body: &str) {
        let dir = duduclaw_core::preset::preset_dir(home, id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("preset.toml"), body).unwrap();
    }

    const PRESET: &str = "[preset]\nversion = \"1.0.0\"\nlabel = \"業務跟進助理\"\n\n\
                           [model]\npreferred = \"claude-haiku-4-5\"\nfallback = \"claude-sonnet-4-6\"\n";

    #[test]
    fn unbound_agent_produces_no_section() {
        let h = home();
        write_agent(h.path(), "bob", "");
        assert_eq!(build_preset_section(h.path(), "bob"), None);
    }

    #[test]
    fn unknown_agent_produces_no_section() {
        let h = home();
        assert_eq!(build_preset_section(h.path(), "ghost"), None);
    }

    #[test]
    fn bound_agent_sees_the_label_version_and_overrides() {
        let h = home();
        write_agent(h.path(), "bob", "[model]\npreferred = \"claude-sonnet-4-6\"\nfallback = \"claude-haiku-4-5\"\naccount_pool = []\n");
        write_preset(h.path(), "sales-followup", PRESET);
        let dir = h.path().join("agents").join("bob");
        duduclaw_core::preset::bind(h.path(), "bob", &dir, "sales-followup", "tester", "test").unwrap();

        let section = build_preset_section(h.path(), "bob").expect("must produce a section");
        assert!(section.contains("業務跟進助理"));
        assert!(section.contains("sales-followup v1.0.0"));
        assert!(section.contains("model.preferred"), "the agent's own model override must be listed");
    }

    #[test]
    fn broken_binding_surfaces_a_degraded_warning_not_silence() {
        let h = home();
        write_agent(h.path(), "bob", "");
        write_preset(h.path(), "sales-followup", PRESET);
        let dir = h.path().join("agents").join("bob");
        duduclaw_core::preset::bind(h.path(), "bob", &dir, "sales-followup", "tester", "test").unwrap();

        // The preset disappears out from under the binding.
        std::fs::remove_dir_all(duduclaw_core::preset::preset_dir(h.path(), "sales-followup")).unwrap();

        let section = build_preset_section(h.path(), "bob").expect("must warn, not go silent");
        assert!(section.contains("⚠️"));
        assert!(section.contains("不是你的錯"));
    }
}
