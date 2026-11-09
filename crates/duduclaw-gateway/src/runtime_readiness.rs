//! Runtime availability in the gateway's own OS identity and mount namespace.
use std::path::{Path, PathBuf};

pub const PROTECTED_SERVICE_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

/// Native Linux protected mode. SR5 round 3 N3: the one durable latch
/// ([`duduclaw_core::protected_mode::active`]: the environment flag OR the
/// root-owned mode marker / the service home's confirmed employee index),
/// never the environment alone.
pub fn is_protected_linux() -> bool {
    duduclaw_core::protected_mode::active()
}

/// Fixed operator guidance. Never copy the operator's CLI credential store.
pub fn protected_runtime_guidance(binary: &str) -> String {
    let install = if binary == "claude" {
        "cd / && sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>".to_string()
    } else {
        format!("請將 {binary} 安裝至 /usr/bin，並確認 duduclaw 可讀取及執行。 / Install {binary} in /usr/bin, readable and executable by duduclaw.")
    };
    let login = if binary == "claude" {
        "\nsudo -u duduclaw -H claude auth login"
    } else { "" };
    format!("受保護 Linux 服務找不到 {binary}。AI CLI 必須安裝於服務帳號可讀取的系統 PATH；ProtectHome=yes 隱藏 /home、/root 與 /run/user。--migrate-from 只遷移 DuDuClaw 資料，不會遷移 AI CLI 或登入資料。\nProtected Linux service cannot resolve {binary}. Install the CLI in the service system PATH; operator-home CLIs and logins are not migrated. Replace <ver> with the required CLI version. -H and --prefix /usr prevent an operator ~/.npmrc prefix from installing back into home.\n{install}{login}")
}

/// Resolve only unit PATH entries. Canonicalization rejects system-path
/// symlinks into directories hidden by ProtectHome/PrivateTmp/the unit.
pub fn protected_binary(binary: &str) -> Option<String> {
    protected_binary_in_path(binary, PROTECTED_SERVICE_PATH)
}

/// Unlike `canonicalize`, reject hidden intermediate symlink targets too:
/// `/usr/bin/claude -> /home/x/cli -> /usr/lib/claude` cannot cross ProtectHome.
pub fn protected_canonical_path(path: &Path) -> Option<PathBuf> {
    protected_canonical_path_with(path, |candidate| {
        let metadata = std::fs::symlink_metadata(candidate)?;
        if metadata.file_type().is_symlink() {
            std::fs::read_link(candidate).map(Some)
        } else { Ok(None) }
    })
}

fn protected_canonical_path_with(
    path: &Path,
    mut read_link: impl FnMut(&Path) -> std::io::Result<Option<PathBuf>>,
) -> Option<PathBuf> {
    if !path.is_absolute() { return None; }
    let mut pending: std::collections::VecDeque<_> = path.components()
        .map(|part| part.as_os_str().to_owned()).collect();
    let mut resolved = PathBuf::from("/");
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "/" || part == "." { continue; }
        if part == ".." { resolved.pop(); continue; }
        resolved.push(part);
        if protected_hidden(&resolved) { return None; }
        if let Some(target) = read_link(&resolved).ok()? {
            links += 1;
            if links > 40 { return None; }
            resolved.pop();
            if target.is_absolute() { resolved = PathBuf::from("/"); }
            for part in target.components().rev() {
                pending.push_front(part.as_os_str().to_owned());
            }
        }
    }
    Some(resolved)
}

fn protected_binary_in_path(binary: &str, path: &str) -> Option<String> {
    if binary.is_empty() || binary.contains('/') { return None; }
    std::env::split_paths(path).filter(|dir| dir.is_absolute()).find_map(|dir| {
        let candidate = dir.join(binary);
        let resolved = protected_canonical_path(&candidate)?;
        if protected_hidden(&candidate) || protected_hidden(&resolved) || !resolved.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(resolved.as_os_str().as_bytes()).ok()?;
            // The gateway already runs as the service account: access checks
            // this identity inside the service's actual mount namespace.
            if unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::X_OK) } != 0 { return None; }
        }
        Some(candidate.to_string_lossy().into_owned())
    })
}

fn protected_hidden(path: &Path) -> bool {
    ["/home", "/root", "/run/user", "/tmp", "/var/tmp", "/run/docker.sock",
        "/var/run/docker.sock", "/run/containerd", "/run/podman", "/var/run/podman", "/run/docker"]
        .iter().any(|hidden| path.starts_with(hidden))
}

pub(crate) fn detect_runtime(id: &str, user_home: &Path) -> Option<String> {
    if !is_protected_linux() { return duduclaw_core::detect_runtime(id, user_home); }
    let spec = duduclaw_core::runtime_catalog::spec_for(id)?;
    std::iter::once(spec.binary).chain(spec.binary_aliases.iter().copied())
        .find_map(protected_binary)
}

/// Include the default/main agent even when its directory has not been created.
/// Other configured agent providers and global utility calls share this report.
pub(crate) fn configured_checks(home: &Path) -> Vec<serde_json::Value> {
    let config = std::fs::read_to_string(home.join("config.toml")).ok()
        .and_then(|text| text.parse::<toml::Value>().ok());
    let default_agent = config.as_ref().and_then(|v| v.get("general"))
        .and_then(|v| v.get("default_agent")).and_then(toml::Value::as_str);
    let mut dirs: Vec<_> = default_agent.map(|id| home.join("agents").join(id)).into_iter().collect();
    if let Ok(entries) = std::fs::read_dir(home.join("agents")) {
        dirs.extend(entries.flatten().map(|entry| entry.path()).filter(|dir| dir.is_dir()));
    }
    if dirs.is_empty() { dirs.push(home.join("agents/dudu")); }
    dirs.sort();
    dirs.dedup();
    let mut providers = vec![crate::runtime_config::global_utility_provider(home)];
    let mut refused = Vec::new();
    for dir in dirs {
        // The dispatch gate refuses removed/invalid selectors before applying
        // parse defaults. Doctor must expose that refusal as well.
        if let Err(message) = crate::runtime_config::read_agent_sections_for_gate(&dir) {
            refused.push(serde_json::json!({
                "name": "inference_runtime_config", "status": "fail",
                "runtime": "configuration", "message": message, "can_repair": false,
            }));
            continue;
        }
        let settings = crate::runtime_config::load_runtime_settings(&dir);
        providers.push(settings.provider);
        providers.extend(settings.fallback);
    }
    providers.sort_by_key(|runtime| runtime.as_str());
    providers.dedup();
    let user_home = PathBuf::from(duduclaw_core::platform::home_dir());
    let mut checks: Vec<_> = providers.into_iter().filter_map(|runtime| {
        let spec = duduclaw_core::runtime_catalog::spec_for(runtime.as_str())?;
        if spec.binary.is_empty() { return None; }
        let found = detect_runtime(spec.id, &user_home);
        let message = if found.is_some() {
            format!("{} CLI 可由目前服務帳號執行。", spec.display_name)
        } else if is_protected_linux() {
            protected_runtime_guidance(spec.binary)
        } else {
            format!("找不到 {} CLI，請安裝 {} 並確認 gateway 的 PATH。 / {} CLI is missing from the gateway environment.", spec.display_name, spec.binary, spec.binary)
        };
        Some(serde_json::json!({
            "name": format!("inference_runtime_{}", spec.id),
            "runtime": spec.id,
            "status": if found.is_some() { "pass" } else { "fail" },
            "message": message,
            "can_repair": false,
        }))
    }).collect();
    checks.extend(refused);
    checks
}

pub(crate) fn log_startup_missing(home: &Path) {
    // Invoked once at gateway startup, never once per failed conversation.
    for check in configured_checks(home) {
        if check["status"] == "fail" {
            tracing::error!(runtime = %check["runtime"].as_str().unwrap_or(""),
                guidance = %check["message"].as_str().unwrap_or(""),
                "Configured inference runtime binary is missing");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_runtime_guidance_contains_system_install_and_service_login() {
        let message = protected_runtime_guidance("claude");
        assert!(message.contains("sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>"));
        assert!(message.contains("sudo -u duduclaw -H claude auth login"));
        assert!(message.contains("不會遷移 AI CLI 或登入資料"));
        assert!(message.contains("Protected Linux service"));
        assert!(!protected_runtime_guidance("codex").contains("claude auth login"));
    }

    #[test]
    fn protected_resolution_refuses_hidden_locations_and_relative_path() {
        for path in ["/home/x/.local/bin/claude", "/root/.npm/bin/claude", "/run/user/1000/claude", "/tmp/claude"] {
            assert!(protected_hidden(Path::new(path)));
        }
        assert!(!protected_hidden(Path::new("/usr/bin/claude")));
        assert!(protected_binary_in_path("claude", ".:/home/x/.local/bin:/root/.local/bin").is_none());
        assert!(protected_binary_in_path("../claude", PROTECTED_SERVICE_PATH).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn protected_canonical_path_rejects_hidden_intermediate_links() {
        let inspect = |path: &Path| -> std::io::Result<Option<PathBuf>> {
            Ok(match path.to_str().unwrap() {
                "/usr/bin/claude" => Some(PathBuf::from("/home/x/cli")),
                "/home/x/cli" => Some(PathBuf::from("/usr/lib/claude")),
                _ => None,
            })
        };
        assert!(protected_canonical_path_with(Path::new("/usr/bin/claude"), inspect).is_none());
        assert_eq!(protected_canonical_path_with(Path::new("/usr/bin/claude"), |path| {
            Ok((path == Path::new("/usr/bin/claude")).then(|| PathBuf::from("../lib/claude")))
        }), Some(PathBuf::from("/usr/lib/claude")));
        assert!(protected_canonical_path_with(Path::new("/usr/bin/claude"), |path| {
            Ok((path == Path::new("/usr/bin/claude")).then(|| PathBuf::from("claude")))
        }).is_none());
    }

    #[test]
    fn configured_runtime_checks_use_default_agent_and_resolved_preset() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        std::fs::write(home.join("config.toml"), "[general]\ndefault_agent = 'primary'\n[runtime]\nutility_provider = 'openai_compat'\n").unwrap();
        let agent = home.join("agents/primary");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[runtime]\nprovider = 'codex'\n").unwrap();
        let checks = configured_checks(home);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["runtime"], "codex");
        let resolved = duduclaw_core::preset::agent_resolved_path(home, "primary");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(resolved, "[runtime]\nprovider = 'grok'\nfallback = 'codex'\n").unwrap();
        let checks = configured_checks(home);
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().any(|check| check["runtime"] == "grok"));
        assert!(checks.iter().any(|check| check["runtime"] == "codex"));
    }
}
