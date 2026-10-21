//! Hermes Agent home (`~/.hermes` or `$HERMES_HOME`, a copy of it, or one
//! `profiles/<name>/` directory).
//!
//! | file                      | slot                                 |
//! |---------------------------|--------------------------------------|
//! | `SOUL.md`                 | SOUL.md                              |
//! | `memories/MEMORY.md`      | memory facts (entries split on `§`)  |
//! | `memories/USER.md`        | wiki「服務對象」                      |
//! | `skills/<name>/SKILL.md`  | skills                               |
//! | `config.yaml` model       | model (Claude ids only)              |
//! | `.env`, `auth.json`       | never read (secrets)                 |
//! | `state.db`, sessions, …   | archived under `imported/`           |
//!
//! Only the directory given is imported; other profiles are listed so the
//! operator can import them one by one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::detect::ConfigKind;
use super::super::ir::{AgentDraft, Doc, ImportBundle};
use super::super::md;
use super::workspace::note_secrets_hidden;
use super::{bundle, leftovers, pick_instructions, skill_dirs};

pub fn read(root: &Path) -> Result<ImportBundle, String> {
    let mut b = bundle(ConfigKind::Hermes);
    let mut consumed: BTreeSet<PathBuf> = BTreeSet::new();
    let mut secrets = Vec::new();
    let mut hidden = Vec::new();
    let take = |p: PathBuf, consumed: &mut BTreeSet<PathBuf>| -> Option<Doc> {
        if !p.is_file() {
            return None;
        }
        consumed.insert(p.clone());
        md::read_text(&p).map(|t| Doc::new(md::rel(root, &p), t))
    };

    let mut a = AgentDraft {
        ref_id: "hermes".into(),
        id_hint: "hermes".into(),
        display_name: "Hermes".into(),
        ..Default::default()
    };
    a.soul = take(root.join("SOUL.md"), &mut consumed);
    if let Some(m) = take(root.join("memories").join("MEMORY.md"), &mut consumed) {
        a.memory.push(m);
    }
    a.user_profile = take(root.join("memories").join("USER.md"), &mut consumed);
    // Project context files Hermes also reads, when someone keeps them here.
    let (instr, skipped) = pick_instructions(root, root, &mut consumed);
    a.instructions = instr;
    a.skipped.extend(skipped);

    let cfg_path = root.join("config.yaml");
    consumed.insert(cfg_path.clone());
    if let Some(cfg) =
        md::read_text(&cfg_path).and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
    {
        a.model = cfg
            .get("model")
            .and_then(|m| {
                m.get("default")
                    .and_then(|d| d.as_str())
                    .or_else(|| m.as_str())
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(p) = cfg.get("agent").and_then(|g| g.get("personalities")) {
            let n = p
                .as_mapping()
                .map(|m| m.len())
                .or_else(|| p.as_sequence().map(|s| s.len()))
                .unwrap_or(0);
            if n > 0 {
                a.ignored_fields.push((
                    "agent.personalities".into(),
                    format!("{n} 組人設預設集未匯入（原檔已保存，可挑一組手動貼進 SOUL）"),
                ));
            }
        }
        for key in ["channels", "gateway", "platforms"] {
            if cfg.get(key).is_some() {
                a.ignored_fields
                    .push((key.into(), "通道與閘道設定不在設定檔匯入範圍".into()));
            }
        }
    }

    let skills = root.join("skills");
    if skills.is_dir() {
        consumed.insert(skills.clone());
        a.skills = skill_dirs(&skills, root);
    }
    let profiles = root.join("profiles");
    if profiles.is_dir() {
        consumed.insert(profiles.clone());
        let names: Vec<String> = std::fs::read_dir(&profiles)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        if !names.is_empty() {
            b.warnings.push(format!(
                "另有 {} 個 profile（{}）未匯入；請把各 profile 資料夾分別壓縮後再匯入",
                names.len(),
                names.join("、")
            ));
        }
    }
    if let Some(first) = a
        .soul
        .as_ref()
        .and_then(|d| d.text.lines().find(|l| l.starts_with("# ")))
    {
        let t = first.trim_start_matches("# ").trim();
        if !t.is_empty() && !t.to_ascii_lowercase().ends_with(".md") {
            a.display_name = t.to_string();
        }
    }
    a.unmapped = leftovers(root, root, &consumed, &mut secrets, &mut hidden);
    b.display_name = a.display_name.clone();
    b.slug_hint = "hermes".into();
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
    fn hermes_home_maps_soul_memory_user_and_skips_secrets() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(
            &r.join("config.yaml"),
            "model:\n  default: anthropic/claude-opus-4.6\nagent:\n  personalities:\n    a: x\n",
        );
        write(&r.join("SOUL.md"), "# 赫米斯\n冷靜");
        write(&r.join("memories/MEMORY.md"), "客戶在台北§偏好 LINE");
        write(&r.join("memories/USER.md"), "Sam 是老闆");
        write(&r.join("skills/x/SKILL.md"), "---\nname: x\n---\n");
        write(&r.join(".env"), "ANTHROPIC_API_KEY=sk-ant-1");
        write(&r.join("auth.json"), "{}");
        write(&r.join("state.db"), "db");
        write(&r.join("profiles/work/SOUL.md"), "x");
        let b = read(r).unwrap();
        let a = &b.agents[0];
        assert_eq!(a.display_name, "赫米斯");
        assert_eq!(a.model.as_deref(), Some("anthropic/claude-opus-4.6"));
        assert_eq!(a.memory.len(), 1);
        assert!(a.user_profile.is_some());
        assert_eq!(a.skills.len(), 1);
        assert!(
            a.ignored_fields
                .iter()
                .any(|(k, _)| k == "agent.personalities")
        );
        assert_eq!(
            a.unmapped
                .iter()
                .map(|u| u.rel.as_str())
                .collect::<Vec<_>>(),
            vec!["state.db"]
        );
        assert!(
            b.warnings
                .iter()
                .any(|w| w.contains(".env") && w.contains("auth.json"))
        );
        assert!(b.warnings.iter().any(|w| w.contains("work")));
    }
}
