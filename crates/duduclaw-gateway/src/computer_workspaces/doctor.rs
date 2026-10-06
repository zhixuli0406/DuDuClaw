//! `duduclaw doctor` row 「電腦操作工作區」 (design §8.5). `Info` in the
//! design is rendered as a Pass whose text says the feature is off (doctor
//! has no Info level).

use std::path::Path;

use duduclaw_core::types::CheckStatus;

use super::{WorkspaceState, WorkspaceStore, config, files};

/// Pure verdict over facts gathered by [`check`].
pub struct Facts {
    pub config: Result<config::WorkspacesConfig, String>,
    pub unix: bool,
    /// `None` = root absent (fine before first use).
    pub root_problem: Option<Option<&'static str>>,
    pub registry: Result<Vec<super::WorkspaceRow>, String>,
    pub attention: Vec<(String, &'static str)>,
    pub runner_known: bool,
    pub free_bytes: Option<u64>,
}

pub fn verdict(f: &Facts) -> (CheckStatus, String) {
    let cfg = match &f.config {
        Err(why) => {
            return (
                CheckStatus::Fail,
                format!("[computer_use.workspaces] 設定無法解析，功能視為關閉：{why}"),
            );
        }
        Ok(c) => c,
    };
    if !cfg.enabled {
        return (
            CheckStatus::Pass,
            "未啟用（[computer_use.workspaces] enabled = false）".into(),
        );
    }
    if !f.unix {
        return (
            CheckStatus::Fail,
            "此平台不支援電腦操作工作區，已停用".into(),
        );
    }
    if let Some(Some(why)) = f.root_problem {
        return (
            CheckStatus::Fail,
            format!("工作區根目錄 computer_workspaces/ 不安全（{why}），掛載一律拒絕"),
        );
    }
    let rows = match &f.registry {
        Err(why) => {
            return (
                CheckStatus::Fail,
                format!("工作區登錄檔 computer_workspaces.db 打不開：{why}"),
            );
        }
        Ok(rows) => rows,
    };
    let mut warns = Vec::new();
    if !f.runner_known {
        warns.push("取不到 Docker daemon id，帶工作區的 session 目前無法啟動".to_string());
    }
    if f.free_bytes.is_some_and(|free| free < cfg.min_free_bytes) {
        warns.push("磁碟剩餘空間低於 min_free_bytes，寫入會被拒絕".to_string());
    }
    for (id, why) in &f.attention {
        warns.push(format!("{id}：{why}"));
    }
    let live: Vec<_> = rows
        .iter()
        .filter(|r| r.state != WorkspaceState::Deleted)
        .collect();
    let mut counts = std::collections::BTreeMap::new();
    for r in &live {
        *counts.entry(r.state.as_str()).or_insert(0) += 1;
    }
    let bytes: i64 = live.iter().map(|r| r.bytes_used).sum();
    let summary = format!(
        "{} 個工作區（{}），共 {bytes} 位元組",
        live.len(),
        counts
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join("、")
    );
    if warns.is_empty() {
        (CheckStatus::Pass, summary)
    } else {
        (
            CheckStatus::Warn,
            format!("{summary}；{}", warns.join("；")),
        )
    }
}

#[cfg(unix)]
fn root_problem(home: &Path) -> Option<Option<&'static str>> {
    use std::os::unix::fs::MetadataExt;
    let root = home.join(super::paths::ROOT_DIR);
    let meta = match std::fs::symlink_metadata(&root) {
        Err(_) => return None,
        Ok(m) => m,
    };
    // SAFETY: geteuid has no preconditions.
    let why = if meta.file_type().is_symlink() {
        Some("是符號連結")
    } else if !meta.is_dir() {
        Some("不是目錄")
    } else if meta.uid() != unsafe { libc::geteuid() } {
        Some("擁有者不是 gateway 使用者")
    } else if meta.mode() & 0o777 != 0o700 {
        Some("權限不是 0700")
    } else {
        None
    };
    Some(why)
}

#[cfg(not(unix))]
fn root_problem(_home: &Path) -> Option<Option<&'static str>> {
    None
}

/// Gather the facts and render the row.
pub async fn check(home: &Path) -> (CheckStatus, String) {
    let config = config::load(home);
    let enabled = config.as_ref().is_ok_and(|c| c.enabled);
    let (registry, attention) = if enabled && cfg!(unix) {
        match WorkspaceStore::open(home).and_then(|s| {
            let rows = s.list_all()?;
            let mut attention: Vec<(String, &'static str)> = rows
                .iter()
                .filter_map(|r| match r.state {
                    WorkspaceState::Deleting => Some((r.workspace_id.clone(), "刪除未完成")),
                    WorkspaceState::FailedCreate => Some((r.workspace_id.clone(), "建立失敗")),
                    WorkspaceState::Orphaned => {
                        Some((r.workspace_id.clone(), "主人已移除，待管理員處理"))
                    }
                    _ => None,
                })
                .collect();
            for id in s.ids_with_event("write_outcome_unknown")? {
                attention.push((id, "有寫入結果無法確認"));
            }
            for id in s.ids_with_event("write_landed_after_fence")? {
                attention.push((id, "有寫入在失去掛載後才落地"));
            }
            for id in s.ids_with_event("data_unreadable")? {
                attention.push((id, "資料目錄讀不到，待確認的寫入暫停對帳"));
            }
            Ok((rows, attention))
        }) {
            Ok((rows, att)) => (Ok(rows), att),
            Err(e) => (Err(format!("{e:?}")), Vec::new()),
        }
    } else {
        (Ok(Vec::new()), Vec::new())
    };
    let runner_known = !enabled || super::current_runner_id(home).await.is_some();
    let facts = Facts {
        unix: cfg!(unix),
        root_problem: root_problem(home),
        free_bytes: files::free_bytes(home),
        config,
        registry,
        attention,
        runner_known,
    };
    verdict(&facts)
}
