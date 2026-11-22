//! Native MCP tables merge across Codex configuration layers. A CLI empty
//! table is not isolation. Inspect unmanaged sources before each launch.
use std::path::{Path, PathBuf};
#[cfg(windows)]
#[path = "windows_config.rs"]
mod windows_config;

#[cfg(test)]
tokio::task_local! {
    pub(crate) static MCP_CONFIG_TEST_PATHS: Vec<PathBuf>;
}

/// Every refusal here is about Codex's own configuration layers on this host
/// (system, managed, profile, project-tree and MDM sources), not about the
/// employee's `.mcp.json`; it carries the engine-local marker so only the
/// explicit runtime-fallback rule can tell the two apart (RM-181).
fn refusal(path: &Path, reason: &str) -> String {
    refusal_at_layer(path, reason, "主機層")
}

fn refusal_at_layer(path: &Path, reason: &str, layer: &str) -> String {
    // Paths are diagnostics, never channel text. Redacting after interpolation
    // could match an interior /home/ and leak the entire preceding prefix.
    tracing::warn!(path = %path.display(), layer, reason, "Codex external configuration refused");
    let filename = path.file_name().and_then(|name| name.to_str()).unwrap_or("config.toml");
    duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(&format!(
        "Codex 外部設定（{layer}）{filename}：{reason}；拒絕啟動。請由管理者移除原生 MCP 設定，改用 DuDuClaw 的代理設定。"))
}

fn check_value(path: &Path, value: &toml::Value, layer: &str) -> Result<(), String> {
    match value {
        toml::Value::Table(table) => {
            if let Some(servers) = table.get("mcp_servers") {
                if !servers.as_table().is_some_and(|servers| servers.is_empty()) {
                    return Err(refusal_at_layer(path, "定義了 mcp_servers", layer));
                }
            }
            // Includes legacy [profiles.<name>.mcp_servers] and profile files.
            for value in table.values() { check_value(path, value, layer)?; }
        }
        toml::Value::Array(values) => for value in values { check_value(path, value, layer)?; },
        _ => {}
    }
    Ok(())
}

fn check_text(path: &Path, text: &str) -> Result<(), String> {
    check_text_in_layer(path, text, "主機層")
}

fn check_text_in_layer(path: &Path, text: &str, layer: &str) -> Result<(), String> {
    let value = text.parse::<toml::Value>().map_err(|_| refusal_at_layer(path, "無法解析 TOML", layer))?;
    check_value(path, &value, layer)
}

fn check_file(path: &Path) -> Result<(), String> {
    check_file_in_layer(path, "主機層")
}

fn check_file_in_layer(path: &Path, layer: &str) -> Result<(), String> {
    // symlink_metadata distinguishes a dangling link from an absent layer.
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(refusal_at_layer(path, "無法檢查設定檔", layer)),
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(path).map_err(|_| refusal_at_layer(path, "無法讀取設定檔", layer))?;
    check_text_in_layer(path, &text, layer)
}

fn check_profiles(home: &Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(home) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(refusal(home, "無法檢查 profile 設定")),
    };
    for entry in entries {
        let entry = entry.map_err(|_| refusal(home, "無法列出 profile 設定"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".config.toml") { check_file(&entry.path())?; }
        if name == "cloud-config-bundle-cache.json" || (name.starts_with("managed-config-") && name.ends_with(".json")) {
            check_cloud_cache(&entry.path())?;
        }
    }
    Ok(())
}

// Match Codex's CFPreferences forced-value API, rather than ordinary defaults.
#[cfg(target_os = "macos")]
fn check_managed_preferences() -> Result<(), String> {
    use base64::Engine;
    use std::ffi::{c_void, c_char};
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(allocator: *const c_void, text: *const c_char, encoding: u32) -> *const c_void;
        fn CFPreferencesAppValueIsForced(key: *const c_void, application: *const c_void) -> u8;
        fn CFPreferencesCopyAppValue(key: *const c_void, application: *const c_void) -> *const c_void;
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringGetLength(value: *const c_void) -> isize;
        fn CFStringGetCString(value: *const c_void, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(value: *const c_void);
    }
    struct OwnedCf(*const c_void);
    impl Drop for OwnedCf { fn drop(&mut self) { if !self.0.is_null() {
        // SAFETY: each handle came from a CoreFoundation Create/Copy API.
        unsafe { CFRelease(self.0); }
    } } }
    const UTF8: u32 = 0x08000100;
    // SAFETY: static NUL-terminated UTF-8 strings, default allocator.
    let application = OwnedCf(unsafe { CFStringCreateWithCString(std::ptr::null(), c"com.openai.codex".as_ptr(), UTF8) });
    for name in [c"config_toml_base64", c"requirements_toml_base64"] {
        let label = format!("com.openai.codex/{} (macOS MDM)", name.to_string_lossy());
        let path = Path::new(&label);
        let key = OwnedCf(unsafe { CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), UTF8) });
        if application.0.is_null() || key.0.is_null() { return Err(refusal(path, "無法讀取 MDM 設定")); }
        // SAFETY: both references are live CFString instances.
        if unsafe { CFPreferencesAppValueIsForced(key.0, application.0) } == 0 { continue; }
        let value = OwnedCf(unsafe { CFPreferencesCopyAppValue(key.0, application.0) });
        if value.0.is_null() || unsafe { CFGetTypeID(value.0) != CFStringGetTypeID() } {
            return Err(refusal(path, "無法讀取 MDM 字串設定"));
        }
        // SAFETY: the value was checked to be a CFString; the output buffer
        // holds up to four UTF-8 bytes per UTF-16 unit plus its NUL terminator.
        let length = unsafe { CFStringGetLength(value.0) };
        if !(0..=16_777_216).contains(&length) { return Err(refusal(path, "MDM 設定過大")); }
        let mut buffer = vec![0u8; length as usize * 4 + 1];
        if unsafe { CFStringGetCString(value.0, buffer.as_mut_ptr().cast(), buffer.len() as isize, UTF8) } == 0 {
            return Err(refusal(path, "無法解析 MDM 字串"));
        }
        let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
        let bytes = base64::engine::general_purpose::STANDARD.decode(buffer[..end].trim_ascii())
            .map_err(|_| refusal(path, "無法解析 MDM base64"))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| refusal(path, "無法解析 MDM TOML"))?;
        check_text(path, text)?;
    }
    Ok(())
}

/// Does `dir` contain a `.git` entry (directory, or the file a worktree or
/// submodule uses)? Fail closed: an entry that cannot be inspected refuses.
fn has_git_marker(dir: &Path) -> Result<bool, String> {
    let marker = dir.join(".git");
    match std::fs::symlink_metadata(&marker) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(refusal_at_layer(&marker, "無法檢查專案範圍", "專案層")),
    }
}

/// RM-181: the project layers Codex itself would consider. Codex searches
/// for project `.codex/config.toml` layers from the working directory upward
/// and stops at the git root (the directory containing `.git`, inclusive);
/// with no git root it considers the working directory only. Walking every
/// ancestor up to `/` used to inspect the OS user's personal
/// `~/.codex/config.toml` whenever the data directory lives under the home
/// directory, refusing every Codex employee for a file Codex never loads as
/// a project layer. A home directory that IS the git root above the employee
/// directory is still in range and still refused.
fn project_layer_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut walked = Vec::new();
    for dir in root.ancestors() {
        walked.push(dir.to_path_buf());
        if has_git_marker(dir)? {
            return Ok(walked);
        }
    }
    Ok(vec![root.to_path_buf()])
}

pub(super) fn check_project_layers(root: &Path, owned_config: Option<&Path>) -> Result<(), String> {
    use duduclaw_core::path_compare;
    let root = path_compare::canonicalize(root).map_err(|_| refusal_at_layer(root, "無法檢查專案設定", "專案層"))?;
    let owned_config = owned_config.map(|path| path_compare::canonicalize(path)
        .map_err(|_| refusal_at_layer(path, "無法檢查代理設定檔", "員工資料夾"))).transpose()?;
    for parent in project_layer_dirs(&root)? {
        let file = parent.join(".codex/config.toml");
        let owned = owned_config.as_deref().map(|owned| is_owned_project_config(&file, owned))
            .transpose().map_err(|_| refusal_at_layer(&file, "無法檢查代理設定檔", "員工資料夾"))?
            .unwrap_or(false);
        if !owned {
            let employee_dir = owned_config.as_deref().and_then(Path::parent).and_then(Path::parent);
            let is_employee = employee_dir.map(|dir| path_compare::equal(dir, &parent))
                .transpose().map_err(|_| refusal_at_layer(&file, "無法檢查專案設定", "專案層"))?
                .unwrap_or(false);
            let layer = if is_employee { "員工資料夾" } else { "專案層" };
            check_file_in_layer(&file, layer)?;
        }
    }
    Ok(())
}

fn is_owned_project_config(file: &Path, owned: &Path) -> std::io::Result<bool> {
    use duduclaw_core::path_compare;
    let candidate = match path_compare::canonicalize(file) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !path_compare::equal(&candidate, owned)? { return Ok(false); }
    // Case-insensitive comparison is conservative for a denial, but cannot
    // grant an exemption on a Windows directory with case sensitivity enabled.
    #[cfg(windows)]
    { Ok(path_compare::file_identity(&candidate)? == path_compare::file_identity(owned)?) }
    #[cfg(not(windows))]
    { Ok(true) }
}

fn check_cloud_cache(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(refusal(path, "無法檢查管理設定快取")),
        Ok(_) => {}
    }
    let bytes = std::fs::read(path).map_err(|_| refusal(path, "無法讀取管理設定快取"))?;
    let cache: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| refusal(path, "無法解析管理設定快取"))?;
    let bundle = cache.pointer("/signed_payload/bundle")
        .ok_or_else(|| refusal(path, "管理設定快取缺少 bundle"))?;
    for kind in ["config_toml", "requirements_toml"] {
        let fragments = bundle.get(kind).and_then(|v| v.get("enterprise_managed"))
            .and_then(|v| v.as_array()).ok_or_else(|| refusal(path, "管理設定快取格式錯誤"))?;
        for fragment in fragments {
            let text = fragment.get("contents").and_then(|v| v.as_str())
                .ok_or_else(|| refusal(path, "管理設定快取缺少 TOML"))?;
            check_text(path, text)?;
        }
    }
    Ok(())
}

pub(crate) fn require_mcp_isolation() -> Result<(), String> {
    #[cfg(test)]
    if let Ok(result) = MCP_CONFIG_TEST_PATHS.try_with(|paths| {
        paths.iter().try_for_each(|path| check_file(path))
    }) { return result; }
    // Documented Unix system and legacy enterprise defaults. Requirements can
    // constrain MCP identity too; do not mistake them for an owned source.
    #[cfg(unix)]
    for path in ["/etc/codex/config.toml", "/etc/codex/managed_config.toml", "/etc/codex/requirements.toml"] {
        check_file(Path::new(path))?;
    }
    #[cfg(windows)]
    for path in windows_config::paths()? { check_file(&path)?; }
    let host_home = std::env::var_os("CODEX_HOME").map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")));
    if let Some(home) = host_home {
        #[cfg(not(unix))]
        check_file(&home.join("managed_config.toml"))?;
        check_profiles(&home)?;
    }
    #[cfg(target_os = "macos")]
    check_managed_preferences()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_owned_config_and_git_walk_accept_equivalent_path_spellings() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("MixedCaseRepo");
        let employee = repo.join("agents/worker");
        std::fs::create_dir_all(employee.join(".codex")).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let owned = employee.join(".codex/config.toml");
        std::fs::write(&owned, "[mcp_servers.duduclaw]\ncommand='owned'").unwrap();
        let canonical = std::fs::canonicalize(&employee).unwrap();
        let upper = PathBuf::from(canonical.to_str().unwrap().to_uppercase());
        check_project_layers(&upper, Some(&owned)).unwrap();
        check_project_layers(&employee, Some(&upper.join(".CODEX/CONFIG.TOML"))).unwrap();
        std::fs::create_dir(repo.join(".codex")).unwrap();
        std::fs::write(repo.join(".codex/config.toml"), "[mcp_servers.foreign]\ncommand='foreign'").unwrap();
        assert!(check_project_layers(&upper, Some(&owned)).is_err());
    }

    #[test]
    fn refusal_never_exposes_host_paths_even_with_spaces_unicode_or_tilde() {
        for path in ["/private/tmp/測試 空間/home/agents/worker/.codex/config.toml",
            "/Users/測試 者/.codex/config.toml", "~/.codex/config.toml"] {
            let error = refusal(Path::new(path), "定義了 mcp_servers");
            let shown = duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&error).unwrap();
            assert!(shown.contains("config.toml"), "{shown}");
            assert!(shown.contains("主機層"), "{shown}");
            assert!(!shown.contains(path), "{shown}");
            assert!(!shown.contains("/"), "{shown}");
        }
    }

    #[test]
    fn sibling_config_refusals_keep_layer_filename_and_reason_without_paths() {
        for layer in ["專案層", "員工資料夾"] {
            for reason in ["定義了 mcp_servers", "無法解析 TOML", "無法讀取設定檔", "無法檢查設定檔"] {
                let error = refusal_at_layer(Path::new("/private/tmp/空 間/.codex/config.toml"), reason, layer);
                let shown = duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&error).unwrap();
                assert!(shown.contains(layer) && shown.contains("config.toml") && shown.contains(reason), "{shown}");
                assert!(!shown.contains("/"), "{shown}");
            }
        }
    }

    #[test]
    fn project_and_employee_checks_report_the_correct_layer() {
        let root = tempfile::tempdir().unwrap();
        let employee = root.path().join("員工 空間");
        std::fs::create_dir_all(&employee).unwrap();
        let owned = employee.join(".codex/config.toml");
        let missing = check_project_layers(&employee, Some(&owned)).unwrap_err();
        assert!(missing.contains("員工資料夾") && missing.contains("config.toml"), "{missing}");
        native_layer(root.path());
        std::fs::write(root.path().join(".git"), "gitdir: fixture").unwrap();
        native_layer(&employee);
        let project = check_project_layers(&employee, Some(&owned)).unwrap_err();
        assert!(project.contains("專案層") && project.contains("config.toml"), "{project}");
        for error in [missing, project] {
            assert!(!error.contains(&root.path().display().to_string()), "{error}");
        }
    }

    #[test]
    fn absent_and_empty_layers_allow_codex() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        check_file(&file).unwrap();
        for text in ["", "model='test'", "[mcp_servers]", "[profiles.safe]\nmodel='test'"] {
            std::fs::write(&file, text).unwrap();
            check_file(&file).unwrap();
        }
    }
    #[test]
    fn system_managed_and_profile_servers_refuse_with_filename() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["config.toml", "managed_config.toml", "work.config.toml"] {
            let file = dir.path().join(name);
            for text in ["[mcp_servers.crm]\ncommand='unsafe'", "[profiles.work.mcp_servers.crm]\nurl='https://invalid'", "mcp_servers=1"] {
                std::fs::write(&file, text).unwrap();
                let error = check_file(&file).unwrap_err();
                assert!(error.contains(name));
                assert!(error.contains("mcp_servers"));
            }
        }
    }
    #[test]
    fn malformed_and_unreadable_layers_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[bad").unwrap();
        assert!(check_file(&file).unwrap_err().contains("無法解析"));
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(check_file(&file).unwrap_err().contains("無法讀取"));
    }
    #[test]
    fn project_layers_are_checked_except_the_owned_proxy_config() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join(".codex");
        std::fs::create_dir(&project).unwrap();
        let file = project.join("config.toml");
        std::fs::write(&file, "[mcp_servers.native]\ncommand='unsafe'").unwrap();
        assert!(check_project_layers(root.path(), None).unwrap_err().contains("config.toml"));
        check_project_layers(root.path(), Some(&file)).unwrap();
        // RM-181: the owned exemption is exact — another file in range still refuses.
        let employee = root.path().join("agents/worker");
        std::fs::create_dir_all(employee.join(".codex")).unwrap();
        std::fs::write(employee.join(".git"), "gitdir: elsewhere").unwrap();
        let owned = employee.join(".codex/config.toml");
        std::fs::write(&owned, "[mcp_servers.duduclaw]\ncommand='/owned'").unwrap();
        check_project_layers(&employee, Some(&owned)).unwrap();
    }

    fn native_layer(dir: &Path) {
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(dir.join(".codex/config.toml"), "[mcp_servers.native]\ncommand='unsafe'").unwrap();
    }

    /// The installed layout: `<home>/.duduclaw/agents/<id>` under a home that
    /// has the OS user's own `~/.codex/config.toml`.
    fn installed_layout() -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        native_layer(home.path());
        let employee = home.path().join(".duduclaw/agents/worker");
        std::fs::create_dir_all(&employee).unwrap();
        (home, employee)
    }

    #[test]
    fn home_codex_config_above_a_non_git_employee_directory_is_not_a_project_layer() {
        let (_home, employee) = installed_layout();
        check_project_layers(&employee, None).unwrap();
    }

    #[test]
    fn home_codex_config_is_in_range_when_the_home_is_the_git_root() {
        for marker_is_dir in [true, false] {
            let (home, employee) = installed_layout();
            let git = home.path().join(".git");
            if marker_is_dir { std::fs::create_dir(&git).unwrap(); } else { std::fs::write(&git, "gitdir: x").unwrap(); }
            let error = check_project_layers(&employee, None).unwrap_err();
            assert!(error.contains("專案層") && error.contains("config.toml"), "{error}");
            assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error));
        }
    }

    #[test]
    fn layers_in_the_employee_directory_or_up_to_the_git_root_refuse() {
        // In the employee directory itself (no git anywhere).
        let (_home, employee) = installed_layout();
        native_layer(&employee);
        assert!(check_project_layers(&employee, None).is_err());
        // Between the employee directory and the git root.
        let (home, employee) = installed_layout();
        std::fs::remove_file(home.path().join(".codex/config.toml")).unwrap();
        std::fs::create_dir(home.path().join(".duduclaw/.git")).unwrap();
        native_layer(&home.path().join(".duduclaw/agents"));
        assert!(check_project_layers(&employee, None).is_err());
        // Above the git root: out of range.
        let (home, employee) = installed_layout();
        std::fs::create_dir(home.path().join(".duduclaw/.git")).unwrap();
        check_project_layers(&employee, None).unwrap();
    }

    /// A project `.codex/config.toml` that is a symbolic link is inspected
    /// through the link (what Codex would read), and a dangling one refuses.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_project_config_is_checked_through_the_link() {
        let (_home, employee) = installed_layout();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(employee.join(".codex")).unwrap();
        let link = employee.join(".codex/config.toml");
        let native = elsewhere.path().join("native.toml");
        std::fs::write(&native, "[mcp_servers.native]\ncommand='unsafe'").unwrap();
        std::os::unix::fs::symlink(&native, &link).unwrap();
        let error = check_project_layers(&employee, None).unwrap_err();
        assert!(error.contains("config.toml"), "{error}");
        std::fs::write(&native, "model='fixture'").unwrap();
        check_project_layers(&employee, None).unwrap();
        std::fs::remove_file(&native).unwrap();
        assert!(check_project_layers(&employee, None).is_err(), "dangling link is not an absent layer");
    }

    /// A working directory that is itself a symbolic link is walked from its
    /// real location: the link's own parents never hide or add a layer.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_working_directory_is_walked_from_its_real_location() {
        let (home, employee) = installed_layout();
        std::fs::create_dir(home.path().join(".git")).unwrap();
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("worker-link");
        std::os::unix::fs::symlink(&employee, &link).unwrap();
        // The real ancestors reach the git root that holds a native layer.
        assert!(check_project_layers(&link, None).is_err());
        // A link placed inside a repository with a native layer, pointing at
        // a clean directory outside it, is judged by the target.
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        native_layer(repo.path());
        let clean = tempfile::tempdir().unwrap();
        let inner = repo.path().join("inner-link");
        std::os::unix::fs::symlink(clean.path(), &inner).unwrap();
        check_project_layers(&inner, None).unwrap();
        // The owned-config exemption compares real paths too.
        let owned = employee.join(".codex/config.toml");
        std::fs::create_dir_all(employee.join(".codex")).unwrap();
        std::fs::write(&owned, "[mcp_servers.duduclaw]\ncommand='/owned'").unwrap();
        std::fs::remove_file(home.path().join(".codex/config.toml")).unwrap();
        check_project_layers(&link, Some(&link.join(".codex/config.toml"))).unwrap();
    }

    /// A `.git` FILE (worktree / submodule) marks the git root where it
    /// stands; its `gitdir:` content is never followed, so it cannot move the
    /// walk to another directory or past the employee directory.
    #[test]
    fn a_git_file_marks_the_root_and_its_content_never_moves_the_walk() {
        let (home, employee) = installed_layout();
        let elsewhere = tempfile::tempdir().unwrap();
        native_layer(elsewhere.path());
        for target in [elsewhere.path().to_path_buf(), home.path().to_path_buf(), PathBuf::from("/")] {
            std::fs::write(employee.join(".git"), format!("gitdir: {}\n", target.display())).unwrap();
            assert_eq!(project_layer_dirs(&std::fs::canonicalize(&employee).unwrap()).unwrap().len(), 1);
            check_project_layers(&employee, None).unwrap();
        }
        // A layer in the employee directory itself is still in range.
        native_layer(&employee);
        assert!(check_project_layers(&employee, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn an_uninspectable_git_marker_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let (home, employee) = installed_layout();
        let locked = home.path().join(".duduclaw/agents");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Running as root bypasses permission bits; only assert when it bites.
        let blocked = std::fs::symlink_metadata(locked.join(".git")).is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound);
        let result = has_git_marker(&locked);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if blocked { assert!(result.is_err()); }
        let _ = employee;
    }
    #[test]
    fn refusals_are_typed_engine_local_spawn_gate_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[mcp_servers.native]\ncommand='unsafe'").unwrap();
        let error = check_file(&file).unwrap_err();
        assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
        assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error));
        let shown = duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&error).unwrap();
        assert!(shown.starts_with("Codex 外部設定"), "{shown}");
    }
    #[test]
    fn cloud_cache_fragments_are_checked_and_unparseable_cache_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cloud-config-bundle-cache.json");
        for (contents, allowed) in [("model='fixture'", true), ("[mcp_servers.native]\ncommand='unsafe'", false), ("[bad", false)] {
            let cache = serde_json::json!({"signed_payload":{"bundle":{
                "config_toml":{"enterprise_managed":[{"contents":contents}]},
                "requirements_toml":{"enterprise_managed":[]}
            }}});
            std::fs::write(&file, cache.to_string()).unwrap();
            assert_eq!(check_cloud_cache(&file).is_ok(), allowed);
        }
        std::fs::write(&file, "{}").unwrap();
        assert!(check_cloud_cache(&file).is_err());
        std::fs::write(&file, "malformed").unwrap();
        assert!(check_cloud_cache(&file).is_err());
    }
    #[test]
    fn profile_directory_scan_detects_native_servers() {
        let dir = tempfile::tempdir().unwrap();
        check_profiles(dir.path()).unwrap();
        std::fs::write(dir.path().join("work.config.toml"), "[mcp_servers.native]\ncommand='unsafe'").unwrap();
        assert!(check_profiles(dir.path()).unwrap_err().contains("work.config.toml"));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn managed_preferences_api_can_inspect_this_host() {
        // Read only: do not create or mutate preferences on the operator host.
        // A refusal is valid on managed machines and must name its source.
        if let Err(error) = check_managed_preferences() { assert!(error.contains("主機層")); }
    }
    #[cfg(unix)]
    #[test]
    fn dangling_layer_is_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::os::unix::fs::symlink(dir.path().join("missing"), &file).unwrap();
        assert!(check_file(&file).is_err());
    }
}
