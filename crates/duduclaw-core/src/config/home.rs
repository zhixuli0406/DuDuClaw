//! Home bootstrap and read-only permission diagnostics shared by entry points.
use std::path::Path;

/// Create a private home (and any missing parents) without changing existing
/// directories. Set the mode at mkdir time, never chmod an existing install.
pub fn create_home_dir(home: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(home)
}

/// Doctor advice only: changing an existing home's permissions is an operator
/// decision, since it can affect other services sharing the directory.
pub fn home_permissions_warning(home: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = match std::fs::symlink_metadata(home) {
            Ok(metadata) => metadata,
            Err(e) => return Some(format!("無法檢查資料目錄 {} 的權限：{e}", home.display())),
        };
        if !metadata.is_dir() {
            return Some(format!("資料目錄 {} 必須是實體目錄，不能是符號連結。", home.display()));
        }
        if metadata.mode() & 0o022 != 0 {
            // POSIX single-quote escaping keeps spaces and metacharacters data.
            let quoted = format!("'{}'", home.display().to_string().replace('\'', "'\\''"));
            return Some(format!("資料目錄 {} 允許群組或其他使用者寫入（{:04o}），附件授權與上傳配額資料庫會拒絕開啟。確認共用需求後，請由擁有者執行 chmod 700 {quoted}；系統不會自動更改既有權限。", home.display(), metadata.mode() & 0o7777));
        }
    }
    #[cfg(not(unix))]
    let _ = home;
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn private_root() -> tempfile::TempDir {
        tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir().unwrap()
    }

    #[test]
    fn new_home_and_missing_parents_are_private() {
        let root = private_root();
        let parent = root.path().join("parent");
        let home = parent.join("home");
        create_home_dir(&home).unwrap();
        for dir in [&parent, &home] {
            assert_eq!(std::fs::metadata(dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert!(home_permissions_warning(&home).is_none());
    }

    #[test]
    fn existing_modes_are_preserved_and_writable_home_is_reported() {
        let home = private_root();
        for mode in [0o700, 0o755, 0o775, 0o777, 0o1777] {
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            create_home_dir(home.path()).unwrap();
            assert_eq!(std::fs::metadata(home.path()).unwrap().permissions().mode() & 0o7777, mode);
            let warning = home_permissions_warning(home.path());
            assert_eq!(warning.is_some(), mode & 0o022 != 0);
            if let Some(warning) = warning { assert!(warning.contains("chmod 700")); }
        }
    }

    #[test]
    fn doctor_quotes_paths_and_reports_unreadable_or_non_directory_home() {
        let root = private_root();
        let home = root.path().join("operator's home $(id)");
        create_home_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(home_permissions_warning(&home).unwrap().contains("operator'\\''s home $(id)'"));
        let missing = root.path().join("missing");
        assert!(home_permissions_warning(&missing).unwrap().contains("無法檢查"));
        assert!(!missing.exists());
        let file = root.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(home_permissions_warning(&file).unwrap().contains("實體目錄"));
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        assert!(home_permissions_warning(&link).unwrap().contains("符號連結"));
    }
}
