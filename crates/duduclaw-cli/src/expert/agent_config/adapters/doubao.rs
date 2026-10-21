//! 豆包「夥伴安裝包」(exported via 詳情頁「…」› 導出夥伴安裝包).
//!
//! There is no public spec; the distributor's sample is the reference:
//!
//! ```text
//! manifest.json                 {"schema_version","type","units":[{"unit_type":"agent","ref":"a1",…}]}
//! agents/<ref>/agent.yml        name / description / avatar
//! agents/<ref>/persona/IDENTITY.md  夥伴檔案   → IDENTITY.md (+ name / emoji)
//! agents/<ref>/persona/SOUL.md      行為準則   → SOUL.md
//! agents/<ref>/persona/AGENTS.md    工作空間主檔 → INSTRUCTIONS.md
//! agents/<ref>/persona/USER.md      使用者檔案 → wiki「服務對象」
//! agents/<ref>/skills/skills.json + <dir>/SKILL.md → skills (Chinese label kept)
//! ```
//!
//! Unknown manifest fields and unit types are kept in the archive and
//! reported, never guessed at.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::detect::{ConfigKind, doubao_agent_dirs};
use super::super::ir::{AgentDraft, Doc, ImportBundle, SkillDraft};
use super::super::md;
use super::workspace::note_secrets_hidden;
use super::{bundle, dir_label, leftovers, skill_dirs};

pub fn read(root: &Path) -> Result<ImportBundle, String> {
    let mut b = bundle(ConfigKind::Doubao);
    let mut consumed: BTreeSet<PathBuf> = BTreeSet::new();
    let mut secrets = Vec::new();
    let mut hidden = Vec::new();

    let refs = manifest_refs(root, &mut b, &mut consumed);
    let mut dirs: Vec<PathBuf> = refs.iter().map(|r| root.join("agents").join(r)).collect();
    for d in doubao_agent_dirs(root) {
        if !dirs.contains(&d) {
            b.warnings.push(format!(
                "agents/{} 沒有列在 manifest.json，仍一併匯入",
                dir_label(&d)
            ));
            dirs.push(d);
        }
    }
    for dir in dirs {
        if !dir.join("agent.yml").is_file() {
            b.warnings.push(format!(
                "找不到 agents/{}/agent.yml，略過這位夥伴",
                dir_label(&dir)
            ));
            continue;
        }
        consumed.insert(dir.clone());
        b.agents
            .push(read_agent(root, &dir, &mut secrets, &mut hidden));
    }
    if b.agents.is_empty() {
        return Err("豆包夥伴安裝包裡沒有可匯入的夥伴（agents/<ref>/agent.yml）".into());
    }

    if let Some(readme) = ["README.md", "readme.md"]
        .iter()
        .map(|f| root.join(f))
        .find(|p| p.is_file())
    {
        consumed.insert(readme.clone());
        b.unmapped.push(super::super::ir::Unmapped {
            rel: md::rel(root, &readme),
            reason: "安裝包說明文件，只保存不匯入".into(),
        });
    }
    let root_left = leftovers(root, root, &consumed, &mut secrets, &mut hidden);
    b.unmapped.extend(root_left);
    note_secrets_hidden(&mut b, &secrets, &hidden);

    b.display_name = if b.agents.len() == 1 {
        b.agents[0].display_name.clone()
    } else {
        dir_label(root)
    };
    b.description = b
        .agents
        .first()
        .map(|a| a.description.clone())
        .unwrap_or_default();
    Ok(b)
}

/// `ref`s of the manifest's agent units, in order. Unknown unit types are
/// reported. `manifest.json` itself is consumed (archived as metadata).
fn manifest_refs(
    root: &Path,
    b: &mut ImportBundle,
    consumed: &mut BTreeSet<PathBuf>,
) -> Vec<String> {
    let path = root.join("manifest.json");
    let Some(raw) = md::read_text(&path) else {
        return Vec::new();
    };
    consumed.insert(path.clone());
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        b.warnings
            .push("manifest.json 無法解析，改依 agents/ 資料夾匯入".into());
        return Vec::new();
    };
    if let Some(ver) = v.get("schema_version").and_then(|x| x.as_str()) {
        b.version = ver.to_string();
        if ver != "1.0" {
            b.warnings.push(format!(
                "安裝包格式版本 {ver} 與已知樣本（1.0）不同，請確認匯入結果"
            ));
        }
    }
    let mut refs = Vec::new();
    for unit in v
        .get("units")
        .and_then(|u| u.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let kind = unit.get("unit_type").and_then(|x| x.as_str()).unwrap_or("");
        let Some(r) = unit.get("ref").and_then(|x| x.as_str()) else {
            continue;
        };
        if kind != "agent" {
            b.warnings
                .push(format!("安裝包單元「{r}」類型 {kind} 尚未支援，未匯入"));
            continue;
        }
        if r.contains('/') || r.contains('\\') || r.starts_with('.') {
            b.warnings.push(format!(
                "安裝包單元名稱「{}」不安全，未匯入",
                r.escape_debug()
            ));
            continue;
        }
        if let Some(t) = unit.pointer("/data/agent_type").and_then(|x| x.as_str())
            && t != "AILY_PERSONAL"
        {
            b.warnings.push(format!(
                "夥伴「{r}」類型 {t} 與已知樣本不同，請確認匯入結果"
            ));
        }
        refs.push(r.to_string());
    }
    refs
}

fn read_agent(
    root: &Path,
    dir: &Path,
    secrets: &mut Vec<String>,
    hidden: &mut Vec<String>,
) -> AgentDraft {
    let ref_id = dir_label(dir);
    let mut consumed: BTreeSet<PathBuf> = BTreeSet::new();
    let yml_path = dir.join("agent.yml");
    consumed.insert(yml_path.clone());
    let meta = md::read_text(&yml_path)
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
        .unwrap_or(serde_yaml::Value::Null);
    let yget = |k: &str| {
        meta.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };

    let persona = dir.join("persona");
    let take = |name: &str, consumed: &mut BTreeSet<PathBuf>| -> Option<Doc> {
        let p = persona.join(name);
        if !p.is_file() {
            return None;
        }
        consumed.insert(p.clone());
        md::read_text(&p).map(|t| Doc::new(md::rel(root, &p), t))
    };
    // The ref (`a1`) means nothing outside the pack: the id comes from the
    // partner's name when it has an ASCII form, else `doubao-<ref>`.
    let mut a = AgentDraft {
        ref_id: ref_id.clone(),
        id_hint: yget("name").unwrap_or("").to_string(),
        display_name: yget("name").unwrap_or(&ref_id).to_string(),
        description: yget("description").unwrap_or("").to_string(),
        ..Default::default()
    };
    a.identity = take("IDENTITY.md", &mut consumed);
    a.soul = take("SOUL.md", &mut consumed);
    a.instructions = take("AGENTS.md", &mut consumed);
    a.user_profile = take("USER.md", &mut consumed);
    a.tools_notes = take("TOOLS.md", &mut consumed);
    a.heartbeat = take("HEARTBEAT.md", &mut consumed);
    if let Some(m) = take("MEMORY.md", &mut consumed) {
        a.memory.push(m);
    }
    if let Some(id) = a.identity.as_ref().map(|d| md::parse_identity(&d.text)) {
        a.emoji = id.emoji;
    }
    if let Some(avatar) = yget("avatar") {
        let present = root.join(avatar).is_file();
        a.ignored_fields.push((
            "avatar".into(),
            if present {
                "頭像圖片已保存，尚未套用".into()
            } else {
                "頭像圖片未包含在安裝包內".into()
            },
        ));
    }

    let skills_root = dir.join("skills");
    if skills_root.is_dir() {
        consumed.insert(skills_root.clone());
        a.skills = skills_with_labels(root, &skills_root, &mut a);
    }
    a.unmapped = leftovers(dir, root, &consumed, secrets, hidden);
    a
}

/// Skill dirs, labelled from `skills.json` (`dir`, `name`, `label`,
/// `version`). A listed dir that is missing is reported; an unlisted dir with
/// a SKILL.md is still imported.
fn skills_with_labels(root: &Path, skills_root: &Path, a: &mut AgentDraft) -> Vec<SkillDraft> {
    let mut found = skill_dirs(skills_root, root);
    let listed: Vec<serde_json::Value> = md::read_text(&skills_root.join("skills.json"))
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    for entry in &listed {
        let s = |k: &str| entry.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let Some(dir) = s("dir") else { continue };
        match found.iter_mut().find(|d| d.name_hint == dir) {
            Some(d) => {
                d.label = s("label");
                d.version = s("version");
                if let Some(n) = s("name") {
                    d.name_hint = n;
                }
            }
            None => a.skipped.push((
                format!("skills/{dir}"),
                format!(
                    "skills.json 列了「{}」但安裝包內沒有這個技能",
                    s("label").unwrap_or(dir.clone())
                ),
            )),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn reads_the_distributor_sample_shape() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(
            &r.join("manifest.json"),
            r#"{"schema_version":"1.0","type":"agent","units":[{"unit_type":"agent","ref":"a1","data":{"agent_type":"AILY_PERSONAL"}}]}"#,
        );
        write(&r.join("README.md"), "# 說明");
        write(
            &r.join("agents/a1/agent.yml"),
            "name: 報告顧問\ndescription: 濃縮報告\navatar: agents/a1/avatar.png\n",
        );
        write(
            &r.join("agents/a1/persona/IDENTITY.md"),
            "# IDENTITY.md - 報告顧問\n\n## 簽名\n\n🧭 報告顧問\n",
        );
        write(&r.join("agents/a1/persona/SOUL.md"), "# SOUL.md - 工作內核");
        write(&r.join("agents/a1/persona/AGENTS.md"), "# 工作規則");
        write(&r.join("agents/a1/persona/USER.md"), "# USER.md");
        write(
            &r.join("agents/a1/skills/skills.json"),
            r#"[{"dir":"report","name":"report","label":"業務彙報","version":"1.0.0"},{"dir":"gone","label":"不見了"}]"#,
        );
        write(
            &r.join("agents/a1/skills/report/SKILL.md"),
            "---\nname: report\n---\n",
        );
        let b = read(r).unwrap();
        assert_eq!(b.agents.len(), 1);
        let a = &b.agents[0];
        assert_eq!(a.display_name, "報告顧問");
        assert_eq!(a.emoji.as_deref(), Some("🧭"));
        assert!(
            a.soul.is_some()
                && a.identity.is_some()
                && a.instructions.is_some()
                && a.user_profile.is_some()
        );
        assert_eq!(a.skills.len(), 1);
        assert_eq!(a.skills[0].label.as_deref(), Some("業務彙報"));
        assert!(a.skipped.iter().any(|(_, r)| r.contains("不見了")));
        assert!(a.ignored_fields.iter().any(|(k, _)| k == "avatar"));
        assert!(b.unmapped.iter().any(|u| u.rel == "README.md"));
        assert!(a.unmapped.is_empty(), "{:?}", a.unmapped);
    }

    #[test]
    fn unsafe_and_unknown_units_are_reported_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(
            &r.join("manifest.json"),
            r#"{"units":[{"unit_type":"agent","ref":"../x"},{"unit_type":"team","ref":"t1"},{"unit_type":"agent","ref":"a2"}]}"#,
        );
        write(&r.join("agents/a2/agent.yml"), "name: 第二位");
        std::fs::create_dir_all(r.join("agents/a2/persona")).unwrap();
        let b = read(r).unwrap();
        assert_eq!(b.agents.len(), 1);
        assert!(b.warnings.iter().any(|w| w.contains("不安全")));
        assert!(b.warnings.iter().any(|w| w.contains("team")));
    }
}
