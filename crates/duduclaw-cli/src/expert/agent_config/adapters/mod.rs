//! One adapter per source format. Each turns a directory into an
//! [`ImportBundle`]; none of them writes, scans or decides conflicts — that
//! is the writer's job, so every format gets the same fences.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::detect::ConfigKind;
use super::ir::{ImportBundle, SkillDraft, Unmapped};
use super::md;

mod native;
mod dify;
mod doubao;
mod hermes;
mod instructions;
mod subagent;
mod workspace;

pub use workspace::read_workspace;

/// Read `root` as `kind`. Errors are human-readable zh-TW.
pub fn read(kind: ConfigKind, root: &Path) -> Result<ImportBundle, String> {
    match kind {
        ConfigKind::DuDuClaw => native::read(root),
        ConfigKind::Doubao => doubao::read(root),
        ConfigKind::OpenClawState => workspace::read_state_dir(root),
        ConfigKind::Workspace => workspace::read_single(root),
        ConfigKind::Hermes => hermes::read(root),
        ConfigKind::Dify => dify::read(root),
        ConfigKind::Subagents => subagent::read(root),
        ConfigKind::Instructions => instructions::read(root),
    }
}

/// Files that hold credentials in the supported formats. Never read, never
/// archived; their presence is reported so the operator knows to set the
/// keys up again inside DuDuClaw.
pub const SECRET_FILE_NAMES: &[&str] = &[
    ".env",
    // Platform configs that carry channel tokens and API keys.
    "openclaw.json",
    "moltbot.json",
    "clawdbot.json",
    "auth.json",
    "auth-profiles.json",
    "credentials.json",
    "secrets.json",
    ".npmrc",
];

pub fn is_secret_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_FILE_NAMES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(name))
        || lower.starts_with(".env.")
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
        || ["creds.json", "oauth.json", "token.json", "tokens.json"].contains(&lower.as_str())
        || [".pem", ".key", ".p12", ".pfx", ".keystore"]
            .iter()
            .any(|e| lower.ends_with(e))
}

/// Directories that hold credentials or conversation logs in the supported
/// formats (OpenClaw `credentials/`, `identity/`; session logs). Never read,
/// never archived.
pub fn is_secret_dir(name: &str) -> bool {
    ["credentials", "identity", "sessions", "secrets"]
        .iter()
        .any(|d| d.eq_ignore_ascii_case(name))
}

/// Skill directories (those holding a `SKILL.md`) directly under `skills_root`.
pub fn skill_dirs(skills_root: &Path, import_root: &Path) -> Vec<SkillDraft> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(skills_root)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| !is_link(p) && p.is_dir() && p.join("SKILL.md").is_file())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs.into_iter()
        .map(|dir| SkillDraft {
            name_hint: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            label: None,
            version: None,
            source: md::rel(import_root, &dir),
            dir,
        })
        .collect()
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(true)
}

/// Every regular file under `dir` (recursively, links skipped) that is not
/// in `consumed` and not under a consumed directory. Secret files are listed
/// in `secrets` instead of the result. Hidden directories (`.git`, …) are
/// skipped and named in `hidden`.
pub fn leftovers(
    dir: &Path,
    import_root: &Path,
    consumed: &BTreeSet<PathBuf>,
    secrets: &mut Vec<String>,
    hidden: &mut Vec<String>,
) -> Vec<Unmapped> {
    let mut out = Vec::new();
    walk(dir, import_root, consumed, secrets, hidden, &mut out, 0);
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn walk(
    dir: &Path,
    root: &Path,
    consumed: &BTreeSet<PathBuf>,
    secrets: &mut Vec<String>,
    hidden: &mut Vec<String>,
    out: &mut Vec<Unmapped>,
    depth: usize,
) {
    if depth > 8 || consumed.contains(dir) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if consumed.contains(&p) || is_link(&p) {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_secret_file(&name) || (p.is_dir() && is_secret_dir(&name)) {
            secrets.push(md::rel(root, &p));
            continue;
        }
        if p.is_dir() {
            if name.starts_with('.') {
                hidden.push(md::rel(root, &p));
                continue;
            }
            walk(&p, root, consumed, secrets, hidden, out, depth + 1);
        } else if p.is_file() {
            out.push(Unmapped {
                rel: md::rel(root, &p),
                reason: "沒有對應的 DuDuClaw 設定，原檔已保存".into(),
            });
        }
    }
}

/// Pick the instruction file by [`super::detect::INSTRUCTION_PRECEDENCE`].
/// Returns `(chosen, skipped)`; a chosen file that only `@`-imports another
/// file in the same directory (Claude Code `@AGENTS.md`) resolves to it.
pub fn pick_instructions(
    dir: &Path,
    import_root: &Path,
    consumed: &mut BTreeSet<PathBuf>,
) -> (Option<super::ir::Doc>, Vec<(String, String)>) {
    let present: Vec<PathBuf> = super::detect::INSTRUCTION_PRECEDENCE
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| p.is_file())
        .collect();
    let mut skipped = Vec::new();
    let mut chosen: Option<super::ir::Doc> = None;
    for p in &present {
        consumed.insert(p.clone());
        let rel = md::rel(import_root, p);
        let Some(text) = md::read_text(p) else {
            skipped.push((rel, "無法以 UTF-8 讀取或檔案過大".into()));
            continue;
        };
        if chosen.is_some() {
            skipped.push((rel, "優先序較低，已改用前一個檔案（原檔已保存）".into()));
            continue;
        }
        let text = resolve_at_import(dir, &text).unwrap_or(text);
        if text.trim().is_empty() {
            skipped.push((rel, "內容是空的".into()));
            continue;
        }
        chosen = Some(super::ir::Doc::new(rel, text));
    }
    if dir.join("CLAUDE.local.md").is_file() {
        consumed.insert(dir.join("CLAUDE.local.md"));
        skipped.push((
            md::rel(import_root, &dir.join("CLAUDE.local.md")),
            "個人本機設定，不匯入".into(),
        ));
    }
    (chosen, skipped)
}

/// A file whose only non-blank lines are `@<file>` imports of sibling files
/// is replaced by those files' text (one level, same directory only).
fn resolve_at_import(dir: &Path, text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() || !lines.iter().all(|l| l.starts_with('@')) {
        return None;
    }
    let mut out = Vec::new();
    for l in lines {
        let name = l.trim_start_matches('@').trim();
        if name.contains('/')
            || name.contains('\\')
            || name.starts_with('.')
            || is_secret_file(name)
        {
            return None;
        }
        out.push(md::read_text(&dir.join(name))?);
    }
    Some(out.join("\n\n"))
}

/// Shared skeleton for a bundle of `kind`.
pub fn bundle(kind: ConfigKind) -> ImportBundle {
    ImportBundle {
        format_label: kind.label().to_string(),
        format_code: kind.code(),
        ..Default::default()
    }
}

/// Folder name of `root` (used as a display-name fallback).
pub fn dir_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
