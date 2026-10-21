//! Plain instruction files — `AGENTS.md` (Codex, Cursor, Jules, Amp, …),
//! `CLAUDE.md`, `GEMINI.md`, `AGENT.md`, `.github/copilot-instructions.md`.
//!
//! Precedence (first present wins, the rest are reported and archived):
//! `AGENTS.md` > `CLAUDE.md` > `GEMINI.md` > `AGENT.md` >
//! `.github/copilot-instructions.md`. A file that only `@`-imports a sibling
//! (`CLAUDE.md` = `@AGENTS.md`) is read through. `CLAUDE.local.md` is
//! personal and skipped. The chosen text becomes INSTRUCTIONS.md of one new
//! employee; persona stays the default SOUL.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::detect::ConfigKind;
use super::super::ir::{AgentDraft, ImportBundle};
use super::workspace::note_secrets_hidden;
use super::{bundle, dir_label, leftovers, pick_instructions};

pub fn read(root: &Path) -> Result<ImportBundle, String> {
    let mut b = bundle(ConfigKind::Instructions);
    let mut consumed: BTreeSet<PathBuf> = BTreeSet::new();
    let (instr, skipped) = pick_instructions(root, root, &mut consumed);
    let Some(instr) = instr else {
        return Err("找到操作指令檔，但內容是空的或無法讀取".into());
    };
    let title = instr
        .text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
        .filter(|t| !t.is_empty() && !t.to_ascii_lowercase().ends_with(".md"));
    let folder = dir_label(root);
    let display = title.unwrap_or_else(|| {
        if folder.is_empty() {
            "imported-agent".into()
        } else {
            folder.clone()
        }
    });

    for p in [
        ".claude/rules",
        ".cursor/rules",
        ".agents/rules",
        ".windsurf/rules",
        ".clinerules",
        ".kiro/steering",
    ] {
        if root.join(p).exists() {
            b.warnings
                .push(format!("{p} 規則目錄尚未支援，原檔不會套用"));
        }
    }
    let mut secrets = Vec::new();
    let mut hidden = Vec::new();
    let mut a = AgentDraft {
        ref_id: folder.clone(),
        id_hint: folder,
        display_name: display.clone(),
        instructions: Some(instr),
        skipped,
        ..Default::default()
    };
    a.unmapped = leftovers(root, root, &consumed, &mut secrets, &mut hidden);
    b.display_name = display;
    b.slug_hint = a.id_hint.clone();
    b.agents.push(a);
    note_secrets_hidden(&mut b, &secrets, &hidden);
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn precedence_agents_over_claude_over_gemini() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().join("my-repo");
        write(&r.join("CLAUDE.md"), "claude");
        write(&r.join("GEMINI.md"), "gemini");
        let b = read(&r).unwrap();
        assert_eq!(b.agents[0].instructions.as_ref().unwrap().text, "claude");
        assert!(b.agents[0].skipped.iter().any(|(s, _)| s == "GEMINI.md"));
        write(&r.join("AGENTS.md"), "# 客服規則\nagents");
        let b = read(&r).unwrap();
        let a = &b.agents[0];
        assert_eq!(a.instructions.as_ref().unwrap().source, "AGENTS.md");
        assert_eq!(a.display_name, "客服規則");
        assert_eq!(a.skipped.len(), 2);
        assert_eq!(a.id_hint, "my-repo");
    }

    #[test]
    fn claude_md_that_only_imports_agents_md_reads_through() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(&r.join("CLAUDE.md"), "@AGENTS.md\n");
        write(&r.join("CLAUDE.local.md"), "mine");
        // AGENTS.md absent → CLAUDE.md chosen, but @-import of a missing file
        // keeps the literal text.
        let b = read(r).unwrap();
        assert_eq!(
            b.agents[0].instructions.as_ref().unwrap().text,
            "@AGENTS.md\n"
        );
        assert!(
            b.agents[0]
                .skipped
                .iter()
                .any(|(s, _)| s == "CLAUDE.local.md")
        );
    }
}
