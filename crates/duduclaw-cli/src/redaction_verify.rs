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
    ToolContext, collect_token_locations,
};
use serde_json::Value;

/// Default tool name for JSON mode — the generic Odoo record reader, which is
/// what `db_field` rules bind to most often.
const DEFAULT_JSON_TOOL: &str = "odoo_search";

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

/// One structured-field hit, for the JSON-mode report table.
struct FieldHit {
    /// RFC-6901 pointer into the synthesised tool result. A `#` separates the
    /// outer pointer from the pointer inside an embedded-JSON leaf.
    pointer: String,
    masked: String,
    rule_id: String,
    category: String,
    token: String,
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
    // A config we cannot parse must NOT silently become "no config" here: the
    // fallback is the built-in `general` profile, so the report would evidence
    // a rule set the deployment does not actually run — the most misleading
    // possible output from a verification command.
    let mut cfg = load_config_from_home(home)?.unwrap_or_default();
    cfg.enabled = true;
    if let Some(p) = profile {
        cfg.profiles = vec![p.to_string()];
    }
    if cfg.profiles.is_empty() {
        cfg.profiles = vec!["general".to_string()];
    }
    // Force scanning on for the verify run: text/CSV rows arrive as
    // user-input and the JSON sample as a tool result, and the point of the
    // command is to evidence the rules regardless of the deployment's
    // per-source policy.
    cfg.sources.user_input = SourceMode::On.into();
    cfg.sources.tool_results = SourceMode::On.into();

    let paths = ManagerPaths::under_home(home);
    let manager = RedactionManager::open(cfg, paths)
        .map_err(|e| DuDuClawError::Config(format!("redaction manager init failed: {e}")))?;
    Ok(Arc::new(manager))
}

/// Parse `config.toml [redaction]` if present.
///
/// `Ok(None)` means "no config.toml" — the fresh-install case the `general`
/// fallback exists for. A config that exists but cannot be read or parsed is
/// an `Err`: reporting evidence gathered from a *different* rule set than the
/// deployment runs is worse than reporting nothing.
fn load_config_from_home(home: &Path) -> Result<Option<RedactionConfig>> {
    let path = home.join("config.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(DuDuClawError::Config(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    #[derive(serde::Deserialize)]
    struct Wrap {
        #[serde(default)]
        redaction: RedactionConfig,
    }
    let parsed: Wrap = toml::from_str(&raw).map_err(|e| {
        DuDuClawError::Config(format!(
            "{} has a malformed [redaction] block: {e}",
            path.display()
        ))
    })?;
    Ok(Some(parsed.redaction))
}

/// Entry point for `duduclaw redaction verify`.
///
/// Two modes, picked from the file extension:
///
/// - `.json` ⇒ **structured** mode. The file is wrapped as a synthetic tool
///   result and run through `redact_value` with a `ToolContext` built from
///   `--tool` / `--arg`, so `db_field` / `json_path` rules are exercised
///   exactly as they would be on a live MCP call.
/// - anything else ⇒ the original line-oriented text / CSV mode.
pub async fn run(
    file: PathBuf,
    profile: Option<String>,
    agent: Option<String>,
    out: Option<PathBuf>,
    tool: Option<String>,
    args: Vec<String>,
) -> Result<()> {
    let home = duduclaw_core::duduclaw_home();

    let content = std::fs::read_to_string(&file)
        .map_err(|e| DuDuClawError::Config(format!("cannot read {}: {e}", file.display())))?;

    let agent_id = match agent {
        Some(a) => a,
        None => crate::mcp::get_default_agent(&home).await,
    };

    if is_json_file(&file) {
        return run_json(
            &home, &file, &content, profile, &agent_id, out, tool, &args,
        );
    }
    if tool.is_some() || !args.is_empty() {
        return Err(DuDuClawError::Config(
            "--tool / --arg only apply to a .json sample (structured field mode)".into(),
        ));
    }

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

/// Is this a JSON sample (structured field mode)?
fn is_json_file(file: &Path) -> bool {
    file.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

/// Parse repeatable `--arg key=value` pairs into the `arguments` object a
/// real MCP call would carry. Values stay verbatim strings: the structured
/// gate compares them with exact equality after stringifying scalars, so
/// `model=res.partner` is the literal an operator writes in the rule.
fn parse_args(raw: &[String]) -> Result<Value> {
    let mut map = serde_json::Map::new();
    for item in raw {
        let Some((key, value)) = item.split_once('=') else {
            return Err(DuDuClawError::Config(format!(
                "--arg must be key=value, got '{item}'"
            )));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(DuDuClawError::Config(format!(
                "--arg has an empty key: '{item}'"
            )));
        }
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
    Ok(Value::Object(map))
}

/// Structured-field mode: run a JSON sample through `redact_value` as if it
/// were a real tool result.
#[allow(clippy::too_many_arguments)]
fn run_json(
    home: &Path,
    file: &Path,
    content: &str,
    profile: Option<String>,
    agent_id: &str,
    out: Option<PathBuf>,
    tool: Option<String>,
    args: &[String],
) -> Result<()> {
    let parsed: Value = serde_json::from_str(content).map_err(|e| {
        DuDuClawError::Config(format!("{} is not valid JSON: {e}", file.display()))
    })?;

    let tool_name = tool.unwrap_or_else(|| DEFAULT_JSON_TOOL.to_string());
    let arg_value = parse_args(args)?;

    let manager = build_verify_manager(home, profile.as_deref())?;
    let session_id = "redaction-verify".to_string();
    let pipeline = manager
        .pipeline(agent_id, Some(session_id.clone()))
        .map_err(|e| DuDuClawError::Config(format!("pipeline build failed: {e}")))?;

    // Synthesise the shape an MCP tool actually returns: the records
    // pretty-printed into `content[0].text`. Exercising that wrapper is the
    // point — it is where the embedded-JSON pass earns its keep.
    let pretty = serde_json::to_string_pretty(&parsed)
        .map_err(|e| DuDuClawError::Config(format!("cannot re-serialise sample: {e}")))?;
    let mut wrapped = serde_json::json!({
        "content": [{"type": "text", "text": pretty}]
    });

    let started = std::time::Instant::now();
    let ctx = ToolContext {
        tool_name: &tool_name,
        args: Some(&arg_value),
    };
    let tokens = pipeline
        .redact_value(&mut wrapped, &ctx)
        .map_err(|e| DuDuClawError::Config(format!("redact_value failed: {e}")))?;
    let elapsed_ms = started.elapsed().as_millis();

    // Reversibility: restore the whole redacted payload once, as the owner.
    let redacted_text = serde_json::to_string(&wrapped)
        .map_err(|e| DuDuClawError::Config(format!("cannot serialise result: {e}")))?;
    let restored = pipeline
        .restore(
            &redacted_text,
            &Caller::owner(agent_id),
            RestoreTarget::UserChannel,
        )
        .map_err(|e| DuDuClawError::Config(format!("restore failed: {e}")))?;

    let mut locations: Vec<(String, String)> = Vec::new();
    collect_token_locations(&wrapped, "", &mut locations);

    let mut hits: Vec<FieldHit> = Vec::with_capacity(locations.len());
    for (pointer, token) in locations {
        // The vault carries the rule that minted the token plus the original;
        // restore alone would not tell us which rule fired.
        let entry = manager
            .vault()
            .lookup_mapping(&token, agent_id, Some(&session_id))
            .map_err(|e| DuDuClawError::Config(format!("vault lookup failed: {e}")))?;
        let Some(entry) = entry else {
            // A token present in the output but absent from the vault would be
            // a real defect; surface it rather than dropping the row.
            hits.push(FieldHit {
                pointer,
                masked: "(unknown)".into(),
                rule_id: "(not in vault)".into(),
                category: "?".into(),
                token,
                reversible: false,
            });
            continue;
        };
        let original = entry.original.unwrap_or_default();
        hits.push(FieldHit {
            pointer,
            masked: mask_display(&original),
            rule_id: entry.rule_id,
            category: entry.category,
            reversible: !original.is_empty() && restored.contains(&original),
            token,
        });
    }

    let reversible_ok = hits.iter().filter(|h| h.reversible).count();
    let report = render_json_report(
        file,
        agent_id,
        &tool_name,
        &arg_value,
        manager.engine().rule_count(),
        tokens.len(),
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
                "  {} field hits · reversibility {}/{} · {} ms",
                hits.len(),
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
fn render_json_report(
    file: &Path,
    agent_id: &str,
    tool_name: &str,
    args: &Value,
    rule_count: usize,
    tokens_written: usize,
    hits: &[FieldHit],
    reversible_ok: usize,
    elapsed_ms: u128,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# 去識別化驗證報告（結構化欄位）");
    let _ = writeln!(s);
    let _ = writeln!(s, "- 檔案：`{}`", file.display());
    let _ = writeln!(s, "- Agent：`{agent_id}`");
    let _ = writeln!(s, "- 模擬工具：`{tool_name}`");
    let _ = writeln!(
        s,
        "- 工具引數：`{}`",
        serde_json::to_string(args).unwrap_or_else(|_| "{}".into())
    );
    let _ = writeln!(s, "- 生效規則數：{rule_count}");
    let _ = writeln!(s, "- 寫入 token 數：{tokens_written}");
    let _ = writeln!(s, "- 命中數：{}", hits.len());
    let _ = writeln!(s, "- 耗時：{elapsed_ms} ms");
    let _ = writeln!(s);

    if hits.is_empty() {
        let _ = writeln!(
            s,
            "> 沒有命中任何規則。若預期應有命中，請確認 `--tool` / `--arg` 是否對得上規則的 \
             `match_tool` / `match_args`，以及欄位名稱是否與工具實際輸出的 key 一致。"
        );
        return s;
    }

    let _ = writeln!(s, "## 命中明細");
    let _ = writeln!(s);
    let _ = writeln!(s, "| 位置（JSON pointer） | 遮罩後 | 規則 | 類別 | Token | 可還原 |");
    let _ = writeln!(s, "|---|---|---|---|---|---|");
    for h in hits {
        let rev = if h.reversible { "✅" } else { "❌" };
        let _ = writeln!(
            s,
            "| `{}` | `{}` | `{}` | {} | `{}` | {} |",
            h.pointer, h.masked, h.rule_id, h.category, h.token, rev
        );
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "## 還原驗證");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "restore OK {reversible_ok}/{}（以 owner 身分還原整份工具回傳，斷言每個原值都回得來）",
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
    fn malformed_config_fails_the_verify_manager_instead_of_falling_back() {
        // The live defect: `.ok()` turned an unparseable `[redaction]` block
        // into "no config", the builder fell back to the built-in `general`
        // profile, and the report proudly evidenced 6 rules the deployment
        // does not run. A verification command that reports the wrong rule set
        // is worse than one that refuses.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            "[redaction]\nenabled = true\n\n[redaction.rules.x]\ntype = \"identity\"\ncategory = \"PERSON\"\npriority = \"not-a-number\"\n",
        )
        .unwrap();

        let err = build_verify_manager(tmp.path(), None)
            .err()
            .expect("a malformed config must not fall back to `general`");
        assert!(
            err.to_string().contains("malformed [redaction] block"),
            "{err}"
        );
    }

    #[test]
    fn missing_config_still_falls_back_to_the_general_profile() {
        // The fresh-install case the fallback exists for stays intact.
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = build_verify_manager(tmp.path(), None)
            .expect("no config.toml ⇒ built-in general profile");
        assert!(manager.engine().rule_count() > 0);
    }

    #[test]
    fn mask_display_is_cjk_safe() {
        assert_eq!(mask_display("王小明"), "王**");
        assert_eq!(mask_display("A"), "*");
        assert_eq!(mask_display(""), "");
        assert_eq!(mask_display("0912345678"), "0*********");
    }

    #[test]
    fn json_mode_is_chosen_by_extension() {
        assert!(is_json_file(Path::new("/tmp/sample.json")));
        assert!(is_json_file(Path::new("/tmp/sample.JSON")));
        assert!(!is_json_file(Path::new("/tmp/sample.csv")));
        assert!(!is_json_file(Path::new("/tmp/sample")));
        // Not an unanchored contains — a .json in the stem is still text mode.
        assert!(!is_json_file(Path::new("/tmp/my.json.csv")));
    }

    #[test]
    fn parse_args_builds_a_string_map() {
        let v = parse_args(&["model=res.partner".into(), "limit=20".into()]).unwrap();
        assert_eq!(v["model"], serde_json::json!("res.partner"));
        assert_eq!(v["limit"], serde_json::json!("20"));
        // Values keep their own '=' signs.
        let v = parse_args(&["domain=[[\"a\",\"=\",1]]".into()]).unwrap();
        assert_eq!(v["domain"], serde_json::json!("[[\"a\",\"=\",1]]"));
        assert!(parse_args(&["noequals".into()]).is_err());
        assert!(parse_args(&["=value".into()]).is_err());
    }

}
