//! Protected-service runtime checks. Never execute migrated code as root.
use super::{refused, Result};
use std::{collections::BTreeMap, path::{Path, PathBuf}};

pub(super) const SERVICE_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

#[derive(Debug, PartialEq)]
pub(super) enum Resolution {
    Found(PathBuf),
    Hidden(PathBuf),
    Missing,
}

fn hidden(path: &Path) -> bool {
    // ProtectHome, PrivateTmp and InaccessiblePaths from the installed unit.
    ["/home", "/root", "/run/user", "/tmp", "/var/tmp", "/run/docker.sock",
        "/var/run/docker.sock", "/run/containerd", "/run/podman", "/var/run/podman",
        "/run/docker"].iter().any(|root| path.starts_with(root))
}

// Inject filesystem/account probes so tests never depend on the host's CLIs.
pub(super) fn resolve(
    binary: &str,
    canonical: impl Fn(&Path) -> Option<PathBuf>,
    accessible: impl Fn(&Path) -> bool,
    operator_home: Option<&Path>,
) -> Resolution {
    let mut hidden_match = None;
    for directory in SERVICE_PATH.split(':') {
        let path = Path::new(directory).join(binary);
        if let Some(target) = canonical(&path) {
            if hidden(&target) {
                hidden_match = Some(target);
            } else if accessible(&path) {
                return Resolution::Found(path);
            }
        }
    }
    // Diagnostic only: these paths never become executable candidates.
    if let Some(home) = operator_home {
        for directory in [".local/bin", ".npm-global/bin", "bin", ".claude/local"] {
            let path = home.join(directory).join(binary);
            if canonical(&path).is_some() {
                hidden_match = Some(path);
                break;
            }
        }
    }
    hidden_match.map(Resolution::Hidden).unwrap_or(Resolution::Missing)
}

fn read_config(path: &Path) -> Result<toml::Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.parse().map_err(|_| refused(format!("invalid runtime config: {}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(toml::Value::Table(Default::default())),
        Err(error) => Err(error.into()),
    }
}

// Every agent primary is required; a configured fallback is checked but optional.
// Do not silently drop retired selectors such as gemini/agy from migrated data.
pub(crate) fn configured(home: &Path) -> Result<BTreeMap<String, bool>> {
    let config = read_config(&home.join("config.toml"))?;
    let default_agent = config.get("general").and_then(|v| v.get("default_agent"))
        .and_then(toml::Value::as_str).filter(|value| !value.is_empty());
    let mut runtimes = BTreeMap::new();
    let utility = config.get("runtime").and_then(|v| v.get("utility_provider"));
    if utility.is_some_and(|v| !v.is_str()) { return Err(refused("invalid runtime utility_provider")); }
    let utility = utility.and_then(toml::Value::as_str).map(str::trim).filter(|value| !value.is_empty()).unwrap_or("claude");
    runtimes.insert(utility.to_ascii_lowercase(), true);
    let mut saw_default = false;
    let mut saw_agent = false;
    let agents = match std::fs::read_dir(home.join("agents")) {
        Ok(agents) => Some(agents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(agents) = agents {
        for entry in agents {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || !entry.path().join("agent.toml").exists() { continue; }
            saw_agent = true;
            let source = read_config(&entry.path().join("agent.toml"))?;
            let resolved = duduclaw_core::preset::agent_resolved_path(home, &entry.file_name().to_string_lossy());
            let doc = if resolved.exists() { read_config(&resolved)? } else { source };
            let section = doc.get("runtime");
            if section.is_some_and(|v| !v.is_table()) { return Err(refused("invalid [runtime] section")); }
            let mut selectors = Vec::new();
            for (field, required) in [("provider", true), ("fallback", false)] {
                let value = section.and_then(|v| v.get(field));
                if value.is_some_and(|v| !v.is_str()) { return Err(refused(format!("invalid runtime {field}"))); }
                let value = value.and_then(toml::Value::as_str).map(str::trim).filter(|value| !value.is_empty()).unwrap_or(if required { "claude" } else { "" });
                if !value.trim().is_empty() { selectors.push((value.trim().to_ascii_lowercase(), required)); }
            }
            for (provider, required) in selectors {
                runtimes.entry(provider).and_modify(|r| *r |= required).or_insert(required);
            }
            saw_default |= default_agent.is_some_and(|id| entry.file_name() == id);
        }
    }
    if !saw_agent || (default_agent.is_some() && !saw_default) { runtimes.insert("claude".into(), true); }
    Ok(runtimes)
}

pub(crate) fn binary_names(provider: &str) -> Result<Vec<&'static str>> {
    if let Some(spec) = duduclaw_core::runtime_catalog::spec_for(provider) {
        if spec.binary.is_empty() { return Ok(Vec::new()); }
        return Ok(std::iter::once(spec.binary).chain(spec.binary_aliases.iter().copied()).collect());
    }
    match provider {
        "gemini" => Ok(vec!["gemini"]),
        "agy" => Ok(vec!["agy"]),
        _ => Err(refused(format!("unknown configured runtime: {provider}"))),
    }
}

pub(super) fn enforce(missing_primary: bool, allow_missing: bool) -> Result<()> {
    if missing_primary && !allow_missing {
        Err(refused("主要 AI CLI 無法供服務使用 / Primary runtime unavailable. 修復後重試；僅要保留安裝結果可加 --allow-missing-runtime（不會撤回已完成的安裝或升級）。"))
    } else { Ok(()) }
}

pub(super) fn missing_message(binary: &str, resolution: &Resolution) -> String {
    let detail = match resolution {
        Resolution::Hidden(path) => format!("僅在操作者家目錄或服務隱藏的位置找到 CLI / CLI found only in operator home or a service-hidden location: {}\n", path.display()),
        _ => String::new(),
    };
    format!("\n========== 警告 / WARNING ==========\n{detail}{}\n==================================", duduclaw_gateway::runtime_readiness::protected_runtime_guidance(binary))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_protected_linux_runtime_path_resolution() {
        let home = Path::new("/home/x");
        assert_eq!(resolve("claude", |p| (p == Path::new("/usr/bin/claude")).then(|| p.into()), |_| true, Some(home)), Resolution::Found("/usr/bin/claude".into()));
        assert_eq!(resolve("claude", |p| (p == Path::new("/home/x/.local/bin/claude")).then(|| p.into()), |_| true, Some(home)), Resolution::Hidden("/home/x/.local/bin/claude".into()));
        assert_eq!(resolve("claude", |_| None, |_| true, Some(home)), Resolution::Missing);
        assert_eq!(resolve("claude", |_| Some("/root/cli/claude".into()), |_| true, None), Resolution::Hidden("/root/cli/claude".into()));
        assert_eq!(resolve("claude", |p| Some(p.into()), |_| false, None), Resolution::Missing);
        for path in ["/run/user/1000/bin", "/home/x/cli", "/root/cli", "/tmp/cli", "/var/tmp/cli"] { assert!(hidden(Path::new(path))); }
        assert!(!hidden(Path::new("/homebrew/bin/claude")));
    }
    #[test]
    fn service_protected_linux_runtime_flag_and_guidance() {
        assert!(enforce(true, false).is_err());
        assert!(enforce(true, true).is_ok());
        assert!(enforce(false, false).is_ok());
        let message = missing_message("claude", &Resolution::Hidden("/home/x/.local/bin/claude".into()));
        for text in ["警告", "WARNING", "operator home", "--prefix /usr", "sudo -H npm install -g", "@anthropic-ai/claude-code@<ver>", "sudo -u duduclaw -H claude auth login"] { assert!(message.contains(text), "{text}: {message}"); }
    }
    #[test]
    fn service_protected_linux_runtime_reads_migrated_agents() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("agents/dudu")).unwrap();
        std::fs::create_dir_all(temp.path().join("agents/dev")).unwrap();
        std::fs::write(temp.path().join("agents/dudu/agent.toml"), "[runtime]\nprovider = 'codex'\n").unwrap();
        std::fs::write(temp.path().join("agents/dev/agent.toml"), "[runtime]\nprovider = 'grok'\nfallback = 'gemini'\n").unwrap();
        assert_eq!(configured(temp.path()).unwrap(), BTreeMap::from([("claude".into(), true), ("codex".into(), true), ("grok".into(), true), ("gemini".into(), false)]));
        // Preset materialization is the effective provider used by the gateway.
        let resolved = duduclaw_core::preset::agent_resolved_path(temp.path(), "dudu");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(&resolved, "[runtime]\nprovider = 'agy'\n").unwrap();
        assert!(configured(temp.path()).unwrap().contains_key("agy"));
        std::fs::remove_file(resolved).unwrap();
        std::fs::write(temp.path().join("config.toml"), "[runtime]\nutility_provider = 'codex'\n").unwrap();
        std::fs::write(temp.path().join("agents/dudu/agent.toml"), "[runtime]\nprovider = '  '\n").unwrap();
        assert!(configured(temp.path()).unwrap().contains_key("claude"));
        std::fs::write(temp.path().join("agents/dudu/agent.toml"), "[runtime]\nprovider = 42\n").unwrap();
        assert!(configured(temp.path()).is_err());
    }
}
