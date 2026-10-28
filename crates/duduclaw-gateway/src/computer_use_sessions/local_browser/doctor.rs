//! Metadata-only diagnostics: never launch Chrome or read a Keychain secret.
use crate::task_sandbox::doctor::Level;
use std::path::Path;

fn headed_playwright(name: &str, server: &serde_json::Value) -> bool {
    let Some(command) = server["command"].as_str() else {
        return false;
    };
    let args = server["args"].as_array();
    let package = |value: &str| {
        value == "@playwright/mcp"
            || value.starts_with("@playwright/mcp@")
            || matches!(
                Path::new(value).file_name().and_then(|name| name.to_str()),
                Some("playwright-mcp" | "mcp-server-playwright")
            )
    };
    let recognized = name.to_ascii_lowercase().contains("playwright")
        || package(command)
        || args.is_some_and(|args| {
            args.iter()
                .filter_map(serde_json::Value::as_str)
                .any(package)
        });
    recognized
        && !args.is_some_and(|args| {
            args.iter()
                .any(|arg| matches!(arg.as_str(), Some("--headless" | "--headless=true")))
        })
}

pub async fn check(home: &Path) -> (Level, String) {
    let config = match super::policy::LocalBrowserConfig::load(home) {
        Ok(config) => config,
        Err(_) => return (Level::Warn, "本機瀏覽器設定無法讀取；功能已拒絕。".into()),
    };
    let mut employees = Vec::new();
    let mut unmanaged = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home.join("agents")) {
        for entry in entries.flatten() {
            let Some(agent) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !duduclaw_core::is_valid_agent_id(&agent)
                || !entry.file_type().is_ok_and(|kind| kind.is_dir())
            {
                continue;
            }
            let capabilities = duduclaw_core::agent_toml::load(&entry.path()).capabilities;
            if capabilities.computer_use_config.local_browser {
                employees.push(agent.clone());
            }
            let headed = std::fs::read_to_string(entry.path().join(".mcp.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|root| root["mcpServers"].as_object().cloned())
                .is_some_and(|servers| {
                    servers
                        .iter()
                        .any(|(name, server)| headed_playwright(name, server))
                });
            if headed {
                unmanaged.push(agent);
            }
        }
    }
    employees.sort();
    unmanaged.sort();
    let enabled = if config.enabled {
        "已開啟"
    } else {
        "預設關閉／已停用"
    };
    let mut warning = !unmanaged.is_empty();
    let browser = match super::launch::browser() {
        Ok(path) => {
            let plist = path
                .parent()
                .and_then(Path::parent)
                .map(|contents| contents.join("Info.plist"));
            let version = match plist {
                Some(plist) => tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    tokio::process::Command::new("/usr/libexec/PlistBuddy")
                        .args(["-c", "Print :CFBundleShortVersionString"])
                        .arg(plist)
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|value| value.trim().to_owned()),
                None => None,
            };
            let acceptable = version
                .as_deref()
                .is_some_and(|value| super::launch::version_supported(&format!("Chrome/{value}")));
            warning |= config.enabled && !acceptable;
            format!(
                "{}，版本 {}（bundle metadata；啟動時另以 CDP 重驗；此 build 最低 {}）",
                if path.to_string_lossy().contains("Microsoft Edge") {
                    "Edge"
                } else {
                    "Chrome"
                },
                version.as_deref().unwrap_or("未知"),
                super::launch::MIN_BROWSER_MAJOR
            )
        }
        Err(_) => {
            warning |= config.enabled;
            "未找到支援的 macOS Chrome／Edge；其他平台拒絕".into()
        }
    };
    let paired = super::super::active_registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upgrade()
        .filter(|sessions| sessions.home.canonicalize().ok() == home.canonicalize().ok())
        .map(|sessions| sessions.companion.ready());
    warning |= config.enabled && paired != Some(true);
    let pairing = match paired {
        Some(true) => "原生 app 已配對且在場",
        Some(false) => "原生 app 未就緒；不能操作",
        None => "此診斷程序無執行中 gateway 的配對證明；請在本機 app 確認",
    };
    let mut message = format!(
        "{enabled}；{browser}。{pairing}。員工開關：{}。",
        if employees.is_empty() {
            "無".into()
        } else {
            employees.join("、")
        }
    );
    #[cfg(unix)]
    {
        let lease = super::machine_lease::path();
        match super::machine_lease::status(&lease) {
            "cleanup_unverified" | "unavailable" => {
                warning = true;
                message.push_str(&format!("本機瀏覽器清理尚未證實，拒絕新啟動。離線操作者須核對專用行程均已結束後才可復原，保留稽核；租約檔：{}。",lease.display()));
            }
            "busy" => message.push_str("本機瀏覽器機器租約正被持有。"),
            _ => {}
        }
    }
    if !unmanaged.is_empty() {
        message.push_str(&format!(
            "未帶 --headless 的 Playwright：{}；不受本機瀏覽器守門。",
            unmanaged.join("、")
        ));
    }
    (if warning { Level::Warn } else { Level::Pass }, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renaming_a_local_playwright_server_does_not_hide_the_doctor_warning() {
        assert!(headed_playwright(
            "browser",
            &serde_json::json!({"command":"npx","args":["-y","@playwright/mcp@latest"]})
        ));
        assert!(!headed_playwright(
            "browser",
            &serde_json::json!({"command":"npx","args":["@playwright/mcp","--headless"]})
        ));
        assert!(!headed_playwright(
            "other",
            &serde_json::json!({"command":"npx","args":["@other/mcp"]})
        ));
    }
    #[tokio::test]
    async fn malformed_configuration_and_unmanaged_headed_playwright_are_not_reported_ready() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("config.toml"),
            "[computer_use.local_browser]\nenabled='true'\n",
        )
        .unwrap();
        assert!(matches!(check(root.path()).await.0, Level::Warn));
        std::fs::write(root.path().join("config.toml"), "").unwrap();
        let agent = root.path().join("agents/alice");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='alice'\n").unwrap();
        std::fs::write(
            agent.join(".mcp.json"),
            r#"{"mcpServers":{"playwright":{"command":"npx","args":["@playwright/mcp"]}}}"#,
        )
        .unwrap();
        let (level, detail) = check(root.path()).await;
        assert!(matches!(level, Level::Warn));
        assert!(detail.contains("Playwright：alice"));
        std::fs::write(agent.join(".mcp.json"),r#"{"mcpServers":{"playwright":{"command":"npx","args":["@playwright/mcp","--headless"]}}}"#).unwrap();
        assert!(!check(root.path()).await.1.contains("Playwright：alice"));
    }
}
