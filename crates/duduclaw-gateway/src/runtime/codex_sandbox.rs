//! Advisory, credential-free Linux employee sandbox preflight.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockCause {
    Seccomp,
    AppArmorMount,
    UserNamespaceRestricted,
    Unknown,
}

/// These signatures diagnose subprocess errors; they never grant permissions.
pub fn classify_stderr(stderr: &str) -> BlockCause {
    if stderr.contains("No permissions to create a new namespace") {
        BlockCause::Seccomp
    } else if stderr.contains("Failed to make / slave") {
        BlockCause::AppArmorMount
    } else if stderr.contains("RTM_NEWADDR") || stderr.contains("setting up uid map") {
        BlockCause::UserNamespaceRestricted
    } else {
        BlockCause::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxProbe {
    Ok,
    Blocked { cause: BlockCause, detail: String },
    NotApplicable,
}

impl SandboxProbe {
    pub fn blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }

    /// Deliberately excludes raw stderr and implementation paths from UI/logs.
    pub fn message(&self, containerized: bool) -> &'static str {
        if self.blocked() && !containerized {
            return "Codex 員工的 shell 指令無法執行；請管理者確認這台主機允許一般使用者建立 user namespace。";
        }
        match self {
            Self::Ok => "Codex 員工的 shell 沙箱可正常啟動。",
            Self::NotApplicable => "不適用：目前平台、未安裝 Codex CLI 或版本尚不支援此檢查。",
            Self::Blocked { cause: BlockCause::Seccomp | BlockCause::AppArmorMount, .. } =>
                "Codex 員工的 shell 指令無法執行；請更新至目前的 compose 檔案，執行 `docker compose up -d` 重建容器。若已是最新 compose 仍出現，代表主機停用了 user namespace。",
            Self::Blocked { cause: BlockCause::UserNamespaceRestricted, .. } =>
                "Codex 員工的 shell 指令無法執行；請在主機執行 `sudo scripts/docker-apparmor-setup.sh`，將印出的設定行加入 `.env`，再執行 `docker compose up -d` 重建容器。",
            Self::Blocked { cause: BlockCause::Unknown, .. } =>
                "Codex 員工的 shell 指令無法執行；原因尚未確認，請管理者檢查主機的沙箱限制。",
        }
    }
}

#[cfg(any(target_os = "linux", test))]
type ProbeCache = std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::OnceCell<SandboxProbe>>>>;
#[cfg(any(target_os = "linux", test))]
static CACHE: std::sync::OnceLock<ProbeCache> = std::sync::OnceLock::new();

/// Replace rather than remove: a running old probe cannot repopulate the cache.
pub(super) fn clear(bin: &str) {
    #[cfg(any(target_os = "linux", test))]
    {
        CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
            .insert(bin.to_owned(), std::sync::Arc::new(tokio::sync::OnceCell::new()));
    }
    #[cfg(not(any(target_os = "linux", test)))]
    let _ = bin;
}

#[cfg(any(target_os = "linux", test))]
async fn cached_probe<F, Fut>(bin: &str, run: F) -> SandboxProbe
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = SandboxProbe>,
{
    let cell = {
        let mut map = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        map.entry(bin.to_owned()).or_default().clone()
    };
    // Unknown failures may be transient; OnceCell leaves errors uninitialized.
    match cell.get_or_try_init(|| async {
        let result = run().await;
        if matches!(result, SandboxProbe::Blocked { cause: BlockCause::Unknown, .. }) {
            Err(result)
        } else {
            Ok(result)
        }
    }).await {
        Ok(result) => result.clone(),
        Err(result) => result,
    }
}

/// Advisory only. Neither spawning employees nor their permission argv depends on this result.
pub async fn probe(bin: &str) -> SandboxProbe {
    #[cfg(target_os = "linux")]
    { cached_probe(bin, || run_probe(bin)).await }
    #[cfg(not(target_os = "linux"))]
    { let _ = bin; SandboxProbe::NotApplicable }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn probe_args(agent_dir: &std::path::Path) -> Result<Vec<String>, String> {
    let mut args = vec!["sandbox".into()];
    args.extend(super::codex::employee_permission_args(agent_dir, None)?);
    args.extend(["-c".into(), "sandbox_mode=\"workspace-write\"".into(), "--".into(), "/bin/true".into()]);
    Ok(args)
}

#[cfg(any(target_os = "linux", all(test, unix)))]
async fn run_probe(bin: &str) -> SandboxProbe {
    let resolved = if std::path::Path::new(bin).is_absolute() {
        std::path::Path::new(bin).exists().then(|| bin.to_owned())
    } else {
        duduclaw_core::which_cli(bin)
    };
    let Some(bin) = resolved else { return SandboxProbe::NotApplicable; };
    let version = super::codex::probe_codex_version_output(&bin).await;
    let Some(version) = version.filter(|v| super::codex::display_version(v).is_some()) else {
        return SandboxProbe::Blocked {
            cause: BlockCause::Unknown,
            detail: "Codex CLI version probe failed or returned an unknown version".into(),
        };
    };
    if !super::codex::version_supports_employee_permissions(&version) {
        return SandboxProbe::NotApplicable;
    }
    // No real home, agent config, auth files, or MCP configuration is consulted.
    let result = async {
        let temp = tempfile::Builder::new().prefix("duduclaw-codex-preflight-").tempdir()
            .map_err(|e| e.to_string())?;
        let root = temp.path().canonicalize().map_err(|e| e.to_string())?;
        let agent = root.join("agent");
        let home = root.join("home");
        let codex_home = home.join(".codex");
        std::fs::create_dir_all(&agent).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&codex_home).map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(&bin);
        duduclaw_core::spawn_env::apply_agent_cli_env_allowlist_keep_namespaces(&mut cmd);
        cmd.env("HOME", &home).env("CODEX_HOME", &codex_home)
            .env("XDG_CONFIG_HOME", &home).env("XDG_DATA_HOME", &home)
            .env("XDG_CACHE_HOME", &home).env("XDG_STATE_HOME", &home)
            .env("TMPDIR", &root).current_dir(&agent).args(probe_args(&agent)?);
        let out = crate::process_group::output(&mut cmd, None, std::time::Duration::from_secs(20))
            .await.map_err(|e| format!("{e:?}"))?;
        Ok::<_, String>(if out.status.success() {
            SandboxProbe::Ok
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            SandboxProbe::Blocked {
                cause: classify_stderr(&stderr),
                detail: duduclaw_core::truncate_bytes(&stderr, 2048).to_owned(),
            }
        })
    }.await;
    result.unwrap_or_else(|detail| SandboxProbe::Blocked { cause: BlockCause::Unknown, detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn codex_sandbox_cache_is_per_path_and_clearable() {
        let bin = "codex-sandbox-cache-test";
        let unknown = SandboxProbe::Blocked { cause: BlockCause::Unknown, detail: "timeout".into() };
        for _ in 0..2 {
            assert_eq!(cached_probe(bin, || async { unknown.clone() }).await, unknown);
        }
        let blocked = SandboxProbe::Blocked { cause: BlockCause::Seccomp, detail: "test".into() };
        assert_eq!(cached_probe(bin, || async { blocked.clone() }).await, blocked);
        assert_eq!(cached_probe(bin, || async { panic!("repeated probe") }).await, blocked);
        assert_eq!(cached_probe("codex-sandbox-other-path", || async { SandboxProbe::Ok }).await, SandboxProbe::Ok);
        super::super::codex::clear_codex_support(bin);
        assert_eq!(cached_probe(bin, || async { SandboxProbe::Ok }).await, SandboxProbe::Ok);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_sandbox_runs_with_isolated_home_and_classifies_real_exit() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("codex-stub");
        std::fs::write(&bin, r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'codex-cli 0.166.0'; exit 0; fi
[ "$1" = sandbox ] || exit 1
[ -d "$CODEX_HOME" ] || exit 1
[ "$CODEX_HOME" = "$HOME/.codex" ] || exit 1
[ ! -e "$CODEX_HOME/config.toml" ] || exit 1
[ ! -e "$CODEX_HOME/auth.json" ] || exit 1
case "$PWD" in */duduclaw-codex-preflight-*/agent) ;; *) exit 1 ;; esac
echo 'bwrap: Failed to make / slave: Permission denied' >&2
exit 1
"#).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(run_probe(bin.to_str().unwrap()).await,
            SandboxProbe::Blocked { cause: BlockCause::AppArmorMount, .. }));
        std::fs::write(&bin, "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'codex-cli 0.166.0'; fi\nexit 0\n").unwrap();
        assert_eq!(run_probe(bin.to_str().unwrap()).await, SandboxProbe::Ok);
        std::fs::write(&bin, "#!/bin/sh\necho 'codex-cli 0.163.0'\n").unwrap();
        assert_eq!(run_probe(bin.to_str().unwrap()).await, SandboxProbe::NotApplicable);
        assert_eq!(run_probe("/nonexistent/codex-preflight").await, SandboxProbe::NotApplicable);
        for script in ["#!/bin/sh\nexit 1\n", "#!/bin/sh\necho unknown\n"] {
            std::fs::write(&bin, script).unwrap();
            assert!(matches!(run_probe(bin.to_str().unwrap()).await,
                SandboxProbe::Blocked { cause: BlockCause::Unknown, .. }));
        }
    }

    #[test]
    fn codex_sandbox_messages_match_environment() {
        for cause in [BlockCause::Seccomp, BlockCause::AppArmorMount,
                      BlockCause::UserNamespaceRestricted, BlockCause::Unknown] {
            let result = SandboxProbe::Blocked { cause, detail: "private stderr".into() };
            assert_eq!(result.message(false), "Codex 員工的 shell 指令無法執行；請管理者確認這台主機允許一般使用者建立 user namespace。");
            let message = result.message(true);
            assert!(!message.contains("private stderr"));
            match cause {
                BlockCause::Seccomp | BlockCause::AppArmorMount => {
                    assert!(message.contains("docker compose up -d"));
                    assert!(message.ends_with("若已是最新 compose 仍出現，代表主機停用了 user namespace。"));
                }
                BlockCause::UserNamespaceRestricted => assert!(message.contains("sudo scripts/docker-apparmor-setup.sh")),
                BlockCause::Unknown => assert!(message.contains("原因尚未確認")),
            }
        }
        for result in [SandboxProbe::Ok, SandboxProbe::NotApplicable] {
            assert_eq!(result.message(false), result.message(true));
        }
    }

    #[test]
    fn codex_sandbox_classifier() {
        for (stderr, expected) in [
            ("bwrap: No permissions to create a new namespace", BlockCause::Seccomp),
            ("bwrap: Failed to make / slave: Permission denied", BlockCause::AppArmorMount),
            ("bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted", BlockCause::UserNamespaceRestricted),
            ("bwrap: setting up uid map: Permission denied", BlockCause::UserNamespaceRestricted),
            ("unexpected failure 員工", BlockCause::Unknown),
            ("", BlockCause::Unknown),
        ] {
            assert_eq!(classify_stderr(stderr), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn codex_sandbox_probe_never_downgrades() {
        let args = probe_args(std::path::Path::new("/tmp/probe/agent")).unwrap();
        assert_eq!(args.first().map(String::as_str), Some("sandbox"));
        assert!(args.iter().any(|a| a == "default_permissions=\"duduclaw-employee\""));
        assert!(args.iter().any(|a| a == "sandbox_mode=\"workspace-write\""));
        assert!(args.iter().any(|a| a.starts_with("permissions.duduclaw-employee.filesystem=")));
        assert!(!args.iter().any(|a| a.contains("danger-full-access") || a.contains("bypass") || a.contains("legacy_landlock")));
        assert_eq!(&args[args.len() - 2..], ["--", "/bin/true"]);
    }
}
