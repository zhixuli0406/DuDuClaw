//! Report root policy errors and unsupported nested policies without following links.
use std::path::{Path, PathBuf};
use crate::types::CheckStatus;

pub type Check = (String, CheckStatus, String);

pub fn checks(home: &Path) -> Vec<Check> {
    let mut roots = vec![home.join("shared/wiki")];
    let mut output = Vec::new();
    let agents = home.join("agents");
    if std::fs::symlink_metadata(&agents).is_ok_and(|m| m.is_dir() && !crate::fs_safety::is_link_metadata(&m)) {
        if let Ok(entries) = std::fs::read_dir(&agents) {
            for entry in entries.flatten() {
                if std::fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_dir() && !crate::fs_safety::is_link_metadata(&m)) {
                    roots.push(entry.path().join("wiki"));
                }
            }
        }
    }
    for root in roots {
        check_root(&root, &mut output);
    }
    if output.is_empty() {
        output.push(("wiki_scope_policy".into(), CheckStatus::Pass, "知識庫根目錄政策可讀取，未發現無效的子資料夾政策。".into()));
    }
    output
}

fn check_root(root: &Path, output: &mut Vec<Check>) {
    let meta = match std::fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            output.push(("wiki_scope_policy".into(), CheckStatus::Fail, format!("{}：{e}", root.display())));
            return;
        }
    };
    if crate::fs_safety::is_link_metadata(&meta) || !meta.is_dir() {
        output.push(("wiki_scope_policy".into(), CheckStatus::Fail, format!("{}：知識庫根目錄不可為連結。", root.display())));
        return;
    }
    let path = root.join(".scope.toml");
    if let Err(error) = crate::scope_policy::load(&path) {
        output.push(("wiki_scope_policy".into(), CheckStatus::Fail,
            format!("{} 無法解析或讀取，知識庫存取已停止：{error}", path.display())));
    }
    let mut pending: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > 20 { continue; }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                output.push(("wiki_scope_policy".into(), CheckStatus::Fail, format!("{}：{error}", directory.display())));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if depth > 0 && entry.file_name() == ".scope.toml" {
                output.push(("nested_wiki_scope_policy".into(), CheckStatus::Warn,
                    format!("{} 位於子資料夾，不會生效；請將規則移至知識庫根目錄的 .scope.toml。", path.display())));
            }
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !crate::fs_safety::is_link_metadata(&m)) {
                pending.push((path, depth + 1));
            }
        }
    }
}

#[cfg(test)]
#[test]
fn rm209_doctor_reports_bad_root_and_ignored_nested_policy() {
    let home = tempfile::tempdir().unwrap();
    let wiki = home.path().join("shared/wiki");
    std::fs::create_dir_all(wiki.join("notes")).unwrap();
    assert!(checks(home.path()).iter().all(|check| check.1 == CheckStatus::Pass));
    std::fs::write(wiki.join(".scope.toml"), "not [ valid toml").unwrap();
    std::fs::write(wiki.join("notes/.scope.toml"), "").unwrap();
    let output = checks(home.path());
    assert!(output.iter().any(|check| check.1 == CheckStatus::Fail && check.2.replace('\\', "/").contains("shared/wiki/.scope.toml")));
    assert!(output.iter().any(|check| check.1 == CheckStatus::Warn && check.2.replace('\\', "/").contains("notes/.scope.toml")));
}
