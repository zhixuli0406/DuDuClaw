//! Product-owned Agent File adapter. The core parser owns schema and frozen-field checks.
use std::path::Path;

use super::super::detect::{ConfigKind, native_files};
use super::super::ir::{AgentDraft, Doc, ImportBundle};
use super::{bundle, md};

pub fn read(root: &Path) -> Result<ImportBundle, String> {
    let mut result = bundle(ConfigKind::DuDuClaw);
    for path in native_files(root) {
        let raw = md::read_text(&path).ok_or("無法讀取 Agent File")?;
        let profile = duduclaw_core::agent_profile::parse(&raw).map_err(|e| e.to_string())?;
        if !crate::is_valid_agent_id(&profile.id) || duduclaw_core::is_reserved_agent_id(&profile.id) {
            return Err("Agent File 的 id 必須是有效的員工代號，不能包含路徑".into());
        }
        let source = md::rel(root, &path);
        result.warnings.extend(profile.warnings);
        let mut ignored_fields = Vec::new();
        for (key, value) in [("role", &profile.role), ("language", &profile.language)] {
            if !value.is_empty() {
                ignored_fields.push((key.into(), format!("可攜描述 {value} 僅供參考，不會更改員工權限或語言設定")));
            }
        }
        if !profile.skills.is_empty() {
            ignored_fields.push(("skills".into(), format!("技能名稱僅供參考，未安裝：{}", profile.skills.join("、"))));
        }
        if !profile.description.is_empty() {
            ignored_fields.push(("description".into(), "描述保留於匯入記錄，不會改寫身分本文".into()));
        }
        result.agents.push(AgentDraft {
            ref_id: profile.id.clone(),
            id_hint: profile.id,
            display_name: profile.name,
            description: profile.description,
            soul: Some(Doc::new(&source, profile.soul)),
            identity: Some(Doc::new(&source, profile.identity)),
            user_profile: Some(Doc::new(&source, profile.user)),
            instructions: Some(Doc::new(&source, profile.rules)),
            contract: Some(Doc::new(&source, profile.contract)),
            settings: profile.settings,
            ignored_fields,
            ..Default::default()
        });
    }
    let first = result.agents.first().ok_or("找不到 Agent File")?;
    result.slug_hint = first.id_hint.clone();
    result.display_name = first.display_name.clone();
    result.description = first.description.clone();
    Ok(result)
}
