//! Department registry helpers for the `departments.*` dashboard RPCs.
//!
//! WP7 made departments *derived* — an agent's `[agent] department` field plus
//! the shared-wiki / shared-skill `departments/<dept>/` sub-trees ARE the
//! department. This module keeps that design (no new store, no schema): a
//! department "exists" when any of those three places references it, and
//! *pre-creating* one for the create-agent dropdown simply materialises its
//! shared-wiki directory (which also gives the department its knowledge space
//! on day one).
//!
//! All names are validated with [`duduclaw_core::is_valid_department`] before
//! touching the filesystem, so a listed/created/removed department can never
//! escape its parent directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One department row for `departments.list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DepartmentInfo {
    pub name: String,
    /// Agents whose `[agent] department` equals this name.
    pub agent_count: usize,
    /// Display names of those agents (UI hover / remove-guard messaging).
    pub members: Vec<String>,
    /// Number of files under `shared/wiki/departments/<name>/` (recursive).
    pub wiki_pages: usize,
    /// Number of entries under `shared/skills/departments/<name>/`.
    pub skills: usize,
}

/// `<home>/shared/wiki/departments`
pub fn wiki_departments_root(home_dir: &Path) -> PathBuf {
    home_dir
        .join("shared")
        .join("wiki")
        .join(duduclaw_core::DEPARTMENTS_NAMESPACE)
}

/// `<home>/shared/skills/departments`
pub fn skills_departments_root(home_dir: &Path) -> PathBuf {
    home_dir
        .join("shared")
        .join("skills")
        .join(duduclaw_core::DEPARTMENTS_NAMESPACE)
}

/// Valid-named subdirectories of `root`. Missing root ⇒ empty (not an error).
fn subdirs(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| duduclaw_core::is_valid_department(n))
        .collect()
}

/// Recursively count regular files under `dir` (0 when absent). Depth-capped
/// defensively — the wiki tree is operator-curated, not adversarial, but a
/// symlink loop must not hang the RPC.
fn count_files(dir: &Path, depth: usize) -> usize {
    if depth > 6 {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => count_files(&e.path(), depth + 1),
            Ok(t) if t.is_file() => 1,
            _ => 0,
        })
        .sum()
}

/// Union of every place a department can be referenced: agent fields (passed
/// in by the caller, which owns the registry) + wiki dirs + skills dirs.
/// `agent_departments` pairs are `(department, agent_display_name)`.
pub fn list_departments(
    home_dir: &Path,
    agent_departments: &[(String, String)],
) -> Vec<DepartmentInfo> {
    let mut by_name: BTreeMap<String, DepartmentInfo> = BTreeMap::new();
    let mut entry = |name: &str, map: &mut BTreeMap<String, DepartmentInfo>| {
        map.entry(name.to_string()).or_insert_with(|| DepartmentInfo {
            name: name.to_string(),
            agent_count: 0,
            members: Vec::new(),
            wiki_pages: 0,
            skills: 0,
        });
    };

    for (dept, member) in agent_departments {
        if !duduclaw_core::is_valid_department(dept) {
            continue;
        }
        entry(dept, &mut by_name);
        let info = by_name.get_mut(dept).expect("just inserted");
        info.agent_count += 1;
        info.members.push(member.clone());
    }

    let wiki_root = wiki_departments_root(home_dir);
    for dept in subdirs(&wiki_root) {
        entry(&dept, &mut by_name);
        by_name.get_mut(&dept).expect("just inserted").wiki_pages =
            count_files(&wiki_root.join(&dept), 0);
    }

    let skills_root = skills_departments_root(home_dir);
    for dept in subdirs(&skills_root) {
        entry(&dept, &mut by_name);
        by_name.get_mut(&dept).expect("just inserted").skills =
            count_files(&skills_root.join(&dept), 0);
    }

    by_name.into_values().collect()
}

/// Materialise a department: create its shared-wiki directory. Errors when the
/// name is invalid or the department already exists anywhere (wiki dir OR an
/// agent already using the name — creating a duplicate would be confusing).
pub fn create_department(
    home_dir: &Path,
    name: &str,
    existing: &[DepartmentInfo],
) -> Result<(), String> {
    if !duduclaw_core::is_valid_department(name) {
        return Err(
            "部門名稱只能使用英數字、'-'、'_'（1–64 字元，將用於檔案路徑）".to_string(),
        );
    }
    if existing.iter().any(|d| d.name == name) {
        return Err(format!("部門「{name}」已存在"));
    }
    let dir = wiki_departments_root(home_dir).join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立部門目錄失敗：{e}"))
}

/// Remove a department. Fail-safe ordering:
/// 1. any agent still assigned ⇒ refuse (reassign them first);
/// 2. wiki pages or skills exist and `force` is false ⇒ refuse with counts;
/// 3. otherwise remove both `departments/<name>` sub-trees.
pub fn remove_department(
    home_dir: &Path,
    name: &str,
    info: &DepartmentInfo,
    force: bool,
) -> Result<(), String> {
    if !duduclaw_core::is_valid_department(name) {
        return Err("部門名稱不合法".to_string());
    }
    if info.agent_count > 0 {
        return Err(format!(
            "部門「{name}」還有 {} 位 AI 員工（{}），請先在員工設定中改派部門再刪除",
            info.agent_count,
            info.members.join("、"),
        ));
    }
    if (info.wiki_pages > 0 || info.skills > 0) && !force {
        return Err(format!(
            "部門「{name}」還有 {} 個知識頁與 {} 個技能，刪除會一併移除；確認請帶 force",
            info.wiki_pages, info.skills,
        ));
    }
    for root in [wiki_departments_root(home_dir), skills_departments_root(home_dir)] {
        let dir = root.join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("刪除部門目錄失敗：{e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn list_unions_agents_wiki_and_skills() {
        let home = tmp_home();
        std::fs::create_dir_all(wiki_departments_root(home.path()).join("sales")).unwrap();
        std::fs::write(
            wiki_departments_root(home.path()).join("sales").join("sop.md"),
            "x",
        )
        .unwrap();
        std::fs::create_dir_all(skills_departments_root(home.path()).join("support")).unwrap();

        let agents = vec![
            ("sales".to_string(), "業務一號".to_string()),
            ("ops".to_string(), "維運".to_string()),
        ];
        let list = list_departments(home.path(), &agents);
        let names: Vec<_> = list.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["ops", "sales", "support"]);
        let sales = list.iter().find(|d| d.name == "sales").unwrap();
        assert_eq!(sales.agent_count, 1);
        assert_eq!(sales.wiki_pages, 1);
        let ops = list.iter().find(|d| d.name == "ops").unwrap();
        assert_eq!(ops.wiki_pages, 0);
    }

    #[test]
    fn list_skips_invalid_names_fail_closed() {
        let home = tmp_home();
        // A traversal-shaped agent field and a dot-dir on disk must both vanish.
        std::fs::create_dir_all(wiki_departments_root(home.path()).join(".hidden")).unwrap();
        let agents = vec![("../escape".to_string(), "evil".to_string())];
        assert!(list_departments(home.path(), &agents).is_empty());
    }

    #[test]
    fn create_then_duplicate_rejected() {
        let home = tmp_home();
        create_department(home.path(), "hr", &[]).unwrap();
        let list = list_departments(home.path(), &[]);
        assert_eq!(list.len(), 1);
        let err = create_department(home.path(), "hr", &list).unwrap_err();
        assert!(err.contains("已存在"), "{err}");
        assert!(create_department(home.path(), "a/b", &[]).is_err());
    }

    #[test]
    fn remove_guards_members_and_content() {
        let home = tmp_home();
        create_department(home.path(), "sales", &[]).unwrap();
        std::fs::write(
            wiki_departments_root(home.path()).join("sales").join("sop.md"),
            "x",
        )
        .unwrap();

        let with_member = DepartmentInfo {
            name: "sales".into(),
            agent_count: 1,
            members: vec!["業務".into()],
            wiki_pages: 1,
            skills: 0,
        };
        assert!(remove_department(home.path(), "sales", &with_member, true)
            .unwrap_err()
            .contains("AI 員工"));

        let no_member = DepartmentInfo { agent_count: 0, members: vec![], ..with_member };
        // Content present, no force ⇒ refuse; force ⇒ removed.
        assert!(remove_department(home.path(), "sales", &no_member, false)
            .unwrap_err()
            .contains("force"));
        remove_department(home.path(), "sales", &no_member, true).unwrap();
        assert!(!wiki_departments_root(home.path()).join("sales").exists());
    }
}
