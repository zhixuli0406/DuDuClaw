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

fn refusal(path: &Path, reason: &str) -> String {
    duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
        "Codex 外部設定 {}：{reason}；拒絕啟動。請由管理者移除原生 MCP 設定，改用 DuDuClaw 的代理設定。", path.display()))
}

fn check_value(path: &Path, value: &toml::Value) -> Result<(), String> {
    match value {
        toml::Value::Table(table) => {
            if let Some(servers) = table.get("mcp_servers") {
                if !servers.as_table().is_some_and(|servers| servers.is_empty()) {
                    return Err(refusal(path, "定義了 mcp_servers"));
                }
            }
            // Includes legacy [profiles.<name>.mcp_servers] and profile files.
            for value in table.values() { check_value(path, value)?; }
        }
        toml::Value::Array(values) => for value in values { check_value(path, value)?; },
        _ => {}
    }
    Ok(())
}

fn check_text(path: &Path, text: &str) -> Result<(), String> {
    let value = text.parse::<toml::Value>().map_err(|_| refusal(path, "無法解析 TOML"))?;
    check_value(path, &value)
}

fn check_file(path: &Path) -> Result<(), String> {
    // symlink_metadata distinguishes a dangling link from an absent layer.
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(refusal(path, "無法檢查設定檔")),
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(path).map_err(|_| refusal(path, "無法讀取設定檔"))?;
    check_text(path, &text)
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

pub(super) fn check_project_layers(root: &Path, owned_config: Option<&Path>) -> Result<(), String> {
    let root = std::fs::canonicalize(root).map_err(|_| refusal(root, "無法檢查專案設定"))?;
    let owned_config = owned_config.map(std::fs::canonicalize).transpose()
        .map_err(|_| refusal(&root, "無法檢查代理設定檔"))?;
    for parent in root.ancestors() {
        let file = parent.join(".codex/config.toml");
        if Some(file.as_path()) != owned_config.as_deref() { check_file(&file)?; }
    }
    Ok(())
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
        if let Err(error) = check_managed_preferences() { assert!(error.contains("com.openai.codex/")); }
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
