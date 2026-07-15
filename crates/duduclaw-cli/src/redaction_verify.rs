//! `duduclaw redaction verify` — evidence report for the redaction pipeline.
//!
//! The client ask (WP2 / meeting §7) was blunt: don't tell me de-identification
//! works, *show* me. This runs a real CSV / text file through the live
//! [`RedactionPipeline`] — same rules, same vault, same tokens a real
//! conversation would produce — and prints a Markdown report:
//!
//! - every hit: masked original (`王**`) × rule id × token × category, per line;
//! - lines with no PII flagged `PASS-THROUGH`;
//! - a reversibility check: each token is restored (owner scope) and asserted to
//!   round-trip back to the original value (`restore OK n/n`).
//!
//! Vault writes are real (tagged as a verify run so GC can reclaim them), which
//! is the whole point — a mock would prove nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_redaction::{
    Caller, ManagerPaths, RedactionConfig, RedactionManager, RestoreTarget, Source, SourceMode,
};

/// One redaction hit, for the report table.
struct Hit {
    line_no: usize,
    masked: String,
    rule_id: String,
    category: String,
    token: String,
    /// Whether restore round-tripped this token back to its original value.
    reversible: bool,
}

/// Mask a matched original value for display: keep the first character, replace
/// the rest with `*` (CJK-safe — operates on chars, not bytes). A single-char
/// value shows just `*` so nothing leaks.
fn mask_display(original: &str) -> String {
    let chars: Vec<char> = original.chars().collect();
    match chars.len() {
        0 => String::new(),
        1 => "*".to_string(),
        n => {
            let mut s = String::new();
            s.push(chars[0]);
            s.extend(std::iter::repeat('*').take(n - 1));
            s
        }
    }
}

/// Build a manager for the verify run. Prefers the profile the operator names;
/// otherwise falls back to whatever `config.toml [redaction]` enables, then to
/// the built-in `general` profile so the tool is useful on a fresh install.
/// `user_input` is forced to `on` so file rows are actually scanned regardless
/// of the deployment's channel policy.
fn build_verify_manager(
    home: &Path,
    profile: Option<&str>,
) -> Result<Arc<RedactionManager>> {
    let mut cfg = load_config_from_home(home).unwrap_or_default();
    cfg.enabled = true;
    if let Some(p) = profile {
        cfg.profiles = vec![p.to_string()];
    }
    if cfg.profiles.is_empty() {
        cfg.profiles = vec!["general".to_string()];
    }
    // Force user-input scanning on for the verify run.
    cfg.sources.user_input = SourceMode::On.into();

    let paths = ManagerPaths::under_home(home);
    let manager = RedactionManager::open(cfg, paths)
        .map_err(|e| DuDuClawError::Config(format!("redaction manager init failed: {e}")))?;
    Ok(Arc::new(manager))
}

/// Parse `config.toml [redaction]` if present.
fn load_config_from_home(home: &Path) -> Option<RedactionConfig> {
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    #[derive(serde::Deserialize)]
    struct Wrap {
        #[serde(default)]
        redaction: RedactionConfig,
    }
    toml::from_str::<Wrap>(&raw).ok().map(|w| w.redaction)
}

/// Entry point for `duduclaw redaction verify`.
pub async fn run(
    file: PathBuf,
    profile: Option<String>,
    agent: Option<String>,
    out: Option<PathBuf>,
) -> Result<()> {
    let home = duduclaw_core::duduclaw_home();

    let content = std::fs::read_to_string(&file)
        .map_err(|e| DuDuClawError::Config(format!("cannot read {}: {e}", file.display())))?;

    let agent_id = match agent {
        Some(a) => a,
        None => crate::mcp::get_default_agent(&home).await,
    };

    let manager = build_verify_manager(&home, profile.as_deref())?;
    // Dedicated verify session so tokens are namespaced and GC-reclaimable.
    let session_id = "redaction-verify".to_string();
    let pipeline = manager
        .pipeline(&agent_id, Some(session_id.clone()))
        .map_err(|e| DuDuClawError::Config(format!("pipeline build failed: {e}")))?;

    let mut hits: Vec<Hit> = Vec::new();
    let mut pass_through_lines = 0usize;
    let mut scanned_lines = 0usize;
    let started = std::time::Instant::now();

    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        scanned_lines += 1;
        let source = Source::UserChannelInput {
            channel_id: "verify".to_string(),
        };

        // Detail matches (original + rule id) come from the engine; the pipeline
        // redact writes to the vault and returns tokens in the same order.
        let matches = manager.engine().apply(line, &source);
        let output = pipeline
            .redact(line, &source)
            .map_err(|e| DuDuClawError::Config(format!("redact failed on line {line_no}: {e}")))?;

        if output.tokens_written.is_empty() {
            pass_through_lines += 1;
            continue;
        }

        // Reversibility: restore the redacted line as the owner and check each
        // original value survives the round-trip.
        let restored = pipeline
            .restore(
                &output.redacted_text,
                &Caller::owner(&agent_id),
                RestoreTarget::UserChannel,
            )
            .map_err(|e| DuDuClawError::Config(format!("restore failed on line {line_no}: {e}")))?;

        for (m, tok) in matches.iter().zip(output.tokens_written.iter()) {
            let reversible = restored.contains(&m.span.original);
            hits.push(Hit {
                line_no,
                masked: mask_display(&m.span.original),
                rule_id: m.rule.id().to_string(),
                category: m.rule.category().to_string(),
                token: tok.as_str().to_string(),
                reversible,
            });
        }
    }

    let elapsed_ms = started.elapsed().as_millis();
    let reversible_ok = hits.iter().filter(|h| h.reversible).count();
    let report = render_report(
        &file,
        &agent_id,
        manager.engine().rule_count(),
        scanned_lines,
        pass_through_lines,
        &hits,
        reversible_ok,
        elapsed_ms,
    );

    match out {
        Some(path) => {
            std::fs::write(&path, &report)
                .map_err(|e| DuDuClawError::Config(format!("cannot write report: {e}")))?;
            println!("Redaction evidence report written to {}", path.display());
            println!(
                "  {} hits across {} lines · reversibility {}/{} · {} ms",
                hits.len(),
                scanned_lines,
                reversible_ok,
                hits.len(),
                elapsed_ms
            );
        }
        None => print!("{report}"),
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_report(
    file: &Path,
    agent_id: &str,
    rule_count: usize,
    scanned_lines: usize,
    pass_through_lines: usize,
    hits: &[Hit],
    reversible_ok: usize,
    elapsed_ms: u128,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# 去識別化驗證報告");
    let _ = writeln!(s);
    let _ = writeln!(s, "- 檔案：`{}`", file.display());
    let _ = writeln!(s, "- Agent：`{agent_id}`");
    let _ = writeln!(s, "- 生效規則數：{rule_count}");
    let _ = writeln!(s, "- 掃描行數：{scanned_lines}（其中 {pass_through_lines} 行無敏感資料）");
    let _ = writeln!(s, "- 命中數：{}", hits.len());
    let _ = writeln!(s, "- 耗時：{elapsed_ms} ms");
    let _ = writeln!(s);

    if hits.is_empty() {
        let _ = writeln!(s, "> 沒有命中任何規則。若預期應有命中，請確認 profile 與規則設定。");
        return s;
    }

    let _ = writeln!(s, "## 命中明細");
    let _ = writeln!(s);
    let _ = writeln!(s, "| 行 | 遮罩後 | 規則 | 類別 | Token | 可還原 |");
    let _ = writeln!(s, "|---|---|---|---|---|---|");
    for h in hits {
        let rev = if h.reversible { "✅" } else { "❌" };
        let _ = writeln!(
            s,
            "| {} | `{}` | `{}` | {} | `{}` | {} |",
            h.line_no, h.masked, h.rule_id, h.category, h.token, rev
        );
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "## 還原驗證");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "restore OK {reversible_ok}/{}（以 owner 身分還原每個 token，斷言可逆回原值）",
        hits.len()
    );
    if reversible_ok != hits.len() {
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "> ⚠️ 有 {} 個 token 未能還原回原值——請檢查 vault TTL 或規則 restore scope。",
            hits.len() - reversible_ok
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_display_is_cjk_safe() {
        assert_eq!(mask_display("王小明"), "王**");
        assert_eq!(mask_display("A"), "*");
        assert_eq!(mask_display(""), "");
        assert_eq!(mask_display("0912345678"), "0*********");
    }
}
