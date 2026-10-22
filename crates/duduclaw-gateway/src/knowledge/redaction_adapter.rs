//! De-identification for cloud embeddings: the operator's `[redaction]`
//! rules (same profiles, custom rules and AI detection as the rest of the
//! gateway) applied to every passage and query before it is sent, with each
//! hit replaced by a fixed `[CATEGORY]` marker. Nothing is restored.
//!
//! Fail closed: redaction switched off or force-disabled, a config that does
//! not parse, rules that do not compile, or no rule at all ⇒ the redactor
//! refuses and the text is not sent. There is no setting that bypasses it.

use std::path::Path;
use std::sync::Arc;

use duduclaw_knowledge::embed::redactor::{TextRedactor, mark_spans};
use duduclaw_redaction::{ManagerPaths, RuleEngine, Source, build_engine};

use crate::redaction_integration::{BootOutcome, classify_redaction_boot, force_disable_active};

/// Tool name recorded on the redaction source (rules may target it).
pub const EMBED_TOOL_NAME: &str = "knowledge_embed";

/// A compiled rule engine used only to find spans.
pub struct EngineRedactor {
    engine: Arc<RuleEngine>,
}

impl TextRedactor for EngineRedactor {
    fn redact(&self, text: &str) -> Result<String, String> {
        let source = Source::ToolResult {
            tool_name: EMBED_TOOL_NAME.to_string(),
        };
        let spans: Vec<(usize, usize, String)> = self
            .engine
            .apply(text, &source)
            .into_iter()
            .map(|m| (m.span.start, m.span.end, m.rule.category().to_string()))
            .collect();
        mark_spans(text, &spans)
    }
}

/// Build the redactor for `home`, or say why there is none.
pub fn build(home: &Path) -> Result<Arc<dyn TextRedactor>, String> {
    if force_disable_active(home) {
        return Err("去識別化目前被強制關閉".to_string());
    }
    let raw = std::fs::read_to_string(home.join("config.toml")).ok();
    let config = match classify_redaction_boot(raw.as_deref()) {
        BootOutcome::Enabled(cfg) => cfg,
        BootOutcome::Disabled => return Err("去識別化（[redaction]）尚未啟用".to_string()),
        BootOutcome::Poisoned(reason) => {
            return Err(format!(
                "去識別化設定有誤：{}",
                duduclaw_core::truncate_chars(&reason, 200)
            ));
        }
    };
    let engine = build_engine(&config, &ManagerPaths::under_home(home)).map_err(|e| {
        format!(
            "去識別化規則無法編譯：{}",
            duduclaw_core::truncate_chars(&e.to_string(), 200)
        )
    })?;
    if engine.rule_count() == 0 {
        return Err("去識別化沒有任何規則".to_string());
    }
    Ok(Arc::new(EngineRedactor {
        engine: Arc::new(engine),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_with(config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        dir
    }

    #[test]
    fn refuses_without_redaction() {
        let h = home_with("[general]\nname = \"x\"\n");
        assert!(build(h.path()).err().unwrap().contains("尚未啟用"));
        let h = home_with("[redaction]\nenabled = false\n");
        assert!(build(h.path()).is_err());
        let h = home_with("not = [toml");
        assert!(
            build(h.path()).is_err(),
            "unparseable config never means 'send'"
        );
    }

    #[test]
    fn force_disable_flag_refuses() {
        let h = home_with("[redaction]\nenabled = true\nprofiles = [\"general\"]\n");
        std::fs::create_dir_all(h.path().join("redaction")).unwrap();
        std::fs::write(h.path().join("redaction/override.flag"), "1").unwrap();
        assert!(build(h.path()).err().unwrap().contains("強制關閉"));
    }

    #[test]
    fn enabled_profile_marks_emails() {
        let h = home_with("[redaction]\nenabled = true\nprofiles = [\"general\"]\n");
        let r = build(h.path()).expect("general profile compiles");
        let out = r.redact("請寄到 amy.chen@example.com 謝謝").unwrap();
        assert!(!out.contains("amy.chen@example.com"), "{out}");
        assert!(out.contains('['), "{out}");
    }
}
