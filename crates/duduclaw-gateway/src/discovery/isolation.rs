//! Command-only confinement. The caller owns spawning, timeout and reaping.
use super::contracts::IsolationBackend;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct ConfinementSpec {
    pub readonly: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub network: bool,
    pub cpu_secs: u64,
    pub memory_bytes: u64,
    pub pids: u32,
    pub allow_unconfined: bool,
    pub operator_identity: bool,
}
impl Default for ConfinementSpec {
    fn default() -> Self {
        Self {
            readonly: vec![],
            writable: vec![],
            network: false,
            cpu_secs: 10,
            memory_bytes: 512 * 1024 * 1024,
            pids: 32,
            allow_unconfined: false,
            operator_identity: false,
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    #[error("no verified confinement backend available")]
    Unavailable,
    #[error("invalid confinement path or resource limit: {0}")]
    Invalid(String),
}
fn quoted(path: &Path) -> Result<String, IsolationError> {
    let value = path
        .to_str()
        .ok_or_else(|| IsolationError::Invalid("non-UTF8 path".into()))?;
    if !path.is_absolute() || value.chars().any(char::is_control) {
        return Err(IsolationError::Invalid(
            "path must be absolute and contain no controls".into(),
        ));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
/// Generate the narrow Seatbelt profile. No global mach-lookup or network grant.
pub fn seatbelt_profile(spec: &ConfinementSpec) -> Result<String, IsolationError> {
    if spec.cpu_secs == 0 || spec.memory_bytes == 0 || spec.pids == 0 {
        return Err(IsolationError::Invalid("zero resource limit".into()));
    }
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process*)\n(allow signal (target same-sandbox))\n(allow sysctl-read)\n(allow file-read* (literal \"/\"))\n",
    );
    // Runtime files contain no operator data. Metadata permission is scoped to
    // ancestors, not the entire host filesystem.
    let mut read = vec![
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/share"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/System"),
        PathBuf::from("/private/var/db/timezone"),
        PathBuf::from("/private/var/select"),
        PathBuf::from("/Library/Developer/CommandLineTools"),
    ];
    read.extend(spec.readonly.iter().cloned());
    read.extend(spec.writable.iter().cloned());
    for path in read {
        let quote = quoted(&path)?;
        profile.push_str(&format!("(allow file-read* (subpath {quote}))\n"));
        for ancestor in path.ancestors().skip(1) {
            profile.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                quoted(ancestor)?
            ));
        }
    }
    for device in ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"] {
        profile.push_str(&format!("(allow file-read* (literal \"{device}\"))\n"));
    }
    // /dev/null is an output sink, not persistent filesystem access.
    profile.push_str("(allow file-write-data (literal \"/dev/null\"))\n");
    for path in &spec.writable {
        profile.push_str(&format!(
            "(allow file-write* (subpath {}))\n",
            quoted(path)?
        ));
    }
    if spec.network {
        profile.push_str("(allow network*)\n");
    }
    Ok(profile)
}
/// Apply limits and confinement while preserving ONLY explicitly supplied env.
/// This never uses duduclaw-sandbox's environment reconstruction path.
pub fn confine_command(
    command: &mut Command,
    spec: &ConfinementSpec,
) -> Result<IsolationBackend, IsolationError> {
    let profile = seatbelt_profile(spec)?;
    let explicit_env = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_os_string(), value.to_os_string())))
        .collect::<Vec<_>>();
    command.env_clear().envs(explicit_env);
    if spec.allow_unconfined && spec.operator_identity {
        #[cfg(unix)]
        apply_limits(command, spec);
        return Ok(IsolationBackend::None);
    }
    for path in spec.readonly.iter().chain(&spec.writable) {
        if !path.is_absolute() || !path.exists() {
            return Err(IsolationError::Invalid("missing confinement path".into()));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !Path::new("/usr/bin/sandbox-exec").is_file() {
            return Err(IsolationError::Unavailable);
        }
        let program = command.get_program().to_os_string();
        let args = command
            .get_args()
            .map(|v| v.to_os_string())
            .collect::<Vec<_>>();
        let cwd = command.get_current_dir().map(Path::to_path_buf);
        let env = command
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| (key.to_os_string(), value.to_os_string()))
            })
            .collect::<Vec<_>>();
        let mut confined = Command::new("/usr/bin/sandbox-exec");
        confined
            .args(["-p", &profile])
            .arg(program)
            .args(args)
            .env_clear()
            .envs(env);
        confined.current_dir(cwd.unwrap_or_else(|| PathBuf::from("/")));
        // std::Command offers no stdio getters. Confinement must precede pipe
        // configuration; callers set stdin/stdout/stderr after this function.
        *command = confined;
        apply_limits(command, spec);
        return Ok(IsolationBackend::Native);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = profile;
        // Landlock's filesystem-only ABI is insufficient for this contract.
        // A verified container is the supported backend on these platforms.
        let _ = command;
        Err(IsolationError::Unavailable)
    }
}
#[cfg(unix)]
fn apply_limits(command: &mut Command, spec: &ConfinementSpec) {
    use std::os::unix::process::CommandExt;
    let cpu = spec.cpu_secs;
    let memory = spec.memory_bytes;
    let pids = spec.pids;
    command.process_group(0);
    unsafe {
        command.pre_exec(move || {
            let cpu = libc::rlimit {
                rlim_cur: cpu as libc::rlim_t,
                rlim_max: cpu as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // NPROC counts every process of this Unix user, not this sandbox.
            // On macOS it would prevent a busy gateway from spawning a single
            // shell child while providing no sandbox-local process ceiling.
            #[cfg(not(target_os = "macos"))]
            {
                let processes = libc::rlimit {
                    rlim_cur: pids as libc::rlim_t,
                    rlim_max: pids as libc::rlim_t,
                };
                if libc::setrlimit(libc::RLIMIT_NPROC, &processes) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            #[cfg(target_os = "macos")]
            let _ = pids;
            #[cfg(not(target_os = "macos"))]
            {
                let address = libc::rlimit {
                    rlim_cur: memory as libc::rlim_t,
                    rlim_max: memory as libc::rlim_t,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &address) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            // Seatbelt has no hard memory/pid enforcement. Evaluators requiring
            // those ceilings select the container backend instead of claiming them.
            #[cfg(target_os = "macos")]
            let _ = memory;
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_profile_denies_network_writes_and_mach_lookup() {
        let spec = ConfinementSpec {
            readonly: vec![PathBuf::from("/opt/policy")],
            ..Default::default()
        };
        let profile = seatbelt_profile(&spec).unwrap();
        assert!(!profile.contains("allow network"));
        assert!(!profile.contains("mach-lookup"));
        assert!(!profile.contains("file-write*"));
        assert!(profile.contains("file-read-metadata (literal \"/opt\")"));
    }
    #[test]
    fn quotes_untrusted_paths_and_refuses_relative_or_control() {
        assert!(quoted(Path::new("relative")).is_err());
        assert!(quoted(Path::new("/tmp/x\n(allow network*)")).is_err());
        assert_eq!(quoted(Path::new("/tmp/a\"b")).unwrap(), "\"/tmp/a\\\"b\"");
    }
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn absence_is_not_an_automatic_unconfined_fallback() {
        let mut command = Command::new("/bin/true");
        assert!(confine_command(&mut command, &ConfinementSpec::default()).is_err());
        assert!(
            confine_command(
                &mut command,
                &ConfinementSpec {
                    allow_unconfined: true,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_process_preserves_explicit_environment_and_cannot_read_canary() {
        let tmp = tempfile::tempdir().unwrap();
        let canary = tmp.path().join("canary");
        std::fs::write(&canary, "secret").unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .env("ONLY_EXPLICIT", "ok")
            .args([
                "-c",
                "test \"$ONLY_EXPLICIT\" = ok && test -z \"$HOME\" && ! cat \"$1\"",
                "sh",
            ])
            .arg(&canary);
        confine_command(&mut command, &ConfinementSpec::default()).unwrap();
        assert!(command.status().unwrap().success());
    }
}
