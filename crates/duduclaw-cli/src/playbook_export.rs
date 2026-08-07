//! `duduclaw playbook export` — gene JSON export CLI (D5=B).
//!
//! Thin CLI wrapper over `duduclaw_gateway::playbook::{list_active, to_gene}`
//! (§1.4 of `commercial/docs/DESIGN-evolution-v3-aee.md`): lists every
//! currently-valid playbook entry for one agent and serializes each as a
//! GEP-gene-shaped JSON document (independent implementation of a JSON
//! shape — see `gene.rs`'s own doc comment; no evolver code or hub client is
//! vendored, per D5=B). Local export/import only — no network I/O.
//!
//! `duduclaw-cli` already depends on `duduclaw-gateway` (single-binary
//! architecture: the CLI hosts the gateway, never the reverse — see
//! CLAUDE.md's dependency-direction note), so this is a direct in-process
//! call. Unlike the eval harness's B1 boundary (gateway → cli, the
//! *forbidden* direction, hence `eval_runner.rs`'s subprocess spawn), no
//! subprocess is needed here.

use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};

/// Flags for `duduclaw playbook export`.
pub struct ExportOptions {
    pub agent: String,
    /// Write the JSON array here. Default: stdout.
    pub out: Option<PathBuf>,
}

/// Entry point for the `playbook export` subcommand.
pub async fn cmd_playbook_export(home: &Path, opts: ExportOptions) -> Result<()> {
    if !duduclaw_core::is_valid_agent_id(&opts.agent) {
        return Err(DuDuClawError::Agent(format!(
            "invalid agent id {:?}: use 1-64 chars of [a-zA-Z0-9_-]",
            opts.agent
        )));
    }

    let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db"))
        .map_err(|e| DuDuClawError::Memory(format!("open memory.db: {e}")))?;

    let genes = export_genes(&engine, &opts.agent).await;

    let json = serde_json::to_string_pretty(&genes)
        .map_err(|e| DuDuClawError::Memory(format!("serialize genes: {e}")))?;

    match &opts.out {
        Some(path) => {
            std::fs::write(path, format!("{json}\n"))
                .map_err(|e| DuDuClawError::Memory(format!("write {}: {e}", path.display())))?;
        }
        None => println!("{json}"),
    }

    if genes.is_empty() {
        eprintln!(
            "{} agent {:?} has no active playbook entries — exported an empty array.",
            console::style("!").yellow(),
            opts.agent,
        );
    } else {
        eprintln!(
            "{} exported {} playbook gene(s) for {:?}{}",
            console::style("✓").green(),
            genes.len(),
            opts.agent,
            match &opts.out {
                Some(p) => format!(" → {}", p.display()),
                None => String::new(),
            },
        );
    }
    Ok(())
}

/// List every active playbook entry for `agent_id` and serialize each as a
/// gene JSON document, patching in the real `entry_id`/`agent_id` this CLI
/// has access to. `to_gene`'s signature (`content, &PlaybookMeta, &RuleStats`)
/// deliberately doesn't carry either id — see its doc comment: "callers that
/// hold the real id/agent_id ... may patch `v["x-duduclaw"]["entry_id"]` /
/// `["agent_id"]` onto the returned value before persisting/exporting it
/// externally." This is that caller.
async fn export_genes(
    engine: &duduclaw_memory::SqliteMemoryEngine,
    agent_id: &str,
) -> Vec<serde_json::Value> {
    duduclaw_gateway::playbook::list_active(engine, agent_id)
        .await
        .iter()
        .map(|(entry, meta, stats)| {
            let mut gene = duduclaw_gateway::playbook::to_gene(&entry.content, meta, stats);
            if let Some(obj) = gene.get_mut("x-duduclaw").and_then(|v| v.as_object_mut()) {
                obj.insert("entry_id".into(), serde_json::json!(entry.id));
                obj.insert("agent_id".into(), serde_json::json!(agent_id));
            }
            gene
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use duduclaw_gateway::playbook::{self, EvalCaseRef, PlaybookCategory, PlaybookDelta};

    fn temp_eval_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let suite = dir.path().join("s");
        std::fs::create_dir(&suite).unwrap();
        std::fs::write(
            suite.join("c.toml"),
            "[case]\nname = \"c\"\nagent = \"a\"\nprompt = \"hi\"\n[judge]\nrubric = \"r\"\n",
        )
        .unwrap();
        dir
    }

    /// Seed one active playbook entry directly into `<home>/memory.db`
    /// (opening + dropping the engine mirrors how a real process would leave
    /// data on disk for a later, separate `cmd_playbook_export` invocation).
    async fn seed_one_entry(home: &Path, agent: &str) {
        let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
        let evals = temp_eval_root();
        let add = PlaybookDelta::Add {
            content: "when in doubt, ask for the order id".to_string(),
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:capability".to_string()],
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            // WP2.8: new Adds must be E1.
            assertions: duduclaw_gateway::playbook::entry::EntryAssertions {
                output_contains: vec!["order id".to_string()],
                ..Default::default()
            },
            strategy: Vec::new(),
            rationale: "seed".to_string(),
        };
        let outcome = playbook::apply_deltas(&engine, agent, vec![add], &[], evals.path(), Utc::now()).await;
        assert_eq!(outcome.applied.len(), 1, "seed delta must apply cleanly");
    }

    #[tokio::test]
    async fn rejects_invalid_agent_id() {
        let home = tempfile::tempdir().unwrap();
        let err = cmd_playbook_export(
            home.path(),
            ExportOptions { agent: "../evil".to_string(), out: None },
        )
        .await
        .unwrap_err();
        assert!(format!("{err}").contains("invalid agent id"));
    }

    #[tokio::test]
    async fn no_entries_exports_empty_array() {
        let home = tempfile::tempdir().unwrap();
        let out = home.path().join("genes.json");
        cmd_playbook_export(
            home.path(),
            ExportOptions { agent: "no-such-agent".to_string(), out: Some(out.clone()) },
        )
        .await
        .unwrap();

        let raw = std::fs::read_to_string(&out).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, serde_json::json!([]), "no entries ⇒ empty array, never fabricated");
    }

    #[tokio::test]
    async fn exports_lossless_gene_json_with_entry_and_agent_id_patched_in() {
        let home = tempfile::tempdir().unwrap();
        let agent = "gene-export-agent";
        seed_one_entry(home.path(), agent).await;

        let out = home.path().join("genes.json");
        cmd_playbook_export(home.path(), ExportOptions { agent: agent.to_string(), out: Some(out.clone()) })
            .await
            .unwrap();

        let raw = std::fs::read_to_string(&out).unwrap();
        let genes: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(genes.len(), 1);
        let gene = &genes[0];

        assert_eq!(gene["gene_schema"], serde_json::json!("1.0"));
        assert_eq!(gene["type"], serde_json::json!("gene"));
        assert_eq!(gene["category"], serde_json::json!("repair"));
        assert_eq!(gene["summary"], serde_json::json!("when in doubt, ask for the order id"));
        assert_eq!(
            gene["validation"],
            serde_json::json!([{"kind": "duduclaw_eval", "ref": "s/c"}])
        );
        // Patched-in ids this CLI has (to_gene's own signature can't carry them).
        assert_eq!(gene["x-duduclaw"]["agent_id"], serde_json::json!(agent));
        let entry_id = gene["x-duduclaw"]["entry_id"].as_str().unwrap();
        assert!(!entry_id.is_empty(), "entry_id must be patched in, not left empty");

        // Round-trips through `from_gene` (the x-duduclaw entry_id/agent_id
        // patch doesn't participate in `from_gene`'s return type, so this
        // just confirms the base gene shape stays lossless).
        let (summary, meta, stats) = playbook::from_gene(gene).unwrap();
        assert_eq!(summary, "when in doubt, ask for the order id");
        assert_eq!(meta.category, PlaybookCategory::Repair);
        assert_eq!(stats.helpful, 1);
    }
}
