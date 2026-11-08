//! One-shot launchd update relaunch. The helper is a separate launchd job,
//! not a child of the gateway that `bootout` will terminate.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};
use super::{LAUNCHD_LABEL, LEGACY_LAUNCHD_LABEL, launchd_plist_path_in, xml_escape};

const PID_ENV: &str = "DUDUCLAW_UPDATE_GATEWAY_PID";
const MANUAL: &str = "Update installed successfully, restart required; run `duduclaw service restart`";
const POLLS: usize = 50;

fn allowed_label(label: &str) -> bool {
    matches!(label, LAUNCHD_LABEL | LEGACY_LAUNCHD_LABEL)
}

fn helper_label(label: &str) -> String { format!("{label}.update-relaunch") }
fn private_dir(home: &Path) -> PathBuf { home.join(".duduclaw-update-relaunch") }
fn helper_path(home: &Path, label: &str) -> PathBuf {
    private_dir(home).join(format!("{}.plist", helper_label(label)))
}
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| line.strip_prefix('\t').unwrap_or(line).strip_prefix(key))
}
fn pid(text: &str) -> Option<u32> { field(text, "pid = ")?.parse().ok() }
fn target(uid: u32, label: &str) -> String { format!("gui/{uid}/{label}") }

#[derive(Debug)]
struct Reply { success: bool, stdout: String, stderr: String }

#[async_trait::async_trait]
trait Commands: Sync {
    async fn run(&self, program: &str, args: &[String]) -> Result<Reply, String>;
    async fn pause(&self) { tokio::time::sleep(std::time::Duration::from_millis(200)).await; }
}

#[cfg(target_os = "macos")]
struct SystemCommands;
#[cfg(target_os = "macos")]
#[async_trait::async_trait]
impl Commands for SystemCommands {
    async fn run(&self, program: &str, args: &[String]) -> Result<Reply, String> {
        let output = tokio::time::timeout(std::time::Duration::from_secs(5),
            tokio::process::Command::new(program).args(args).kill_on_drop(true).output())
            .await.map_err(|_| format!("{program} timed out"))?
            .map_err(|e| format!("{program}: {e}"))?;
        Ok(Reply { success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned() })
    }
}

async fn ctl(c: &impl Commands, args: &[String]) -> Result<Reply, String> {
    c.run("/bin/launchctl", args).await
}
fn checked(reply: Reply) -> Result<Reply, String> {
    if reply.success { Ok(reply) } else { Err(reply.stderr) }
}
fn missing(reply: &Reply) -> bool {
    // launchctl's diagnostic line includes the exact requested service target.
    !reply.success && reply.stderr.lines().any(|line| {
        line.trim().strip_prefix("Could not find service \"").is_some()
    })
}

/// Neither an XPC environment variable nor a leftover plist proves ownership.
/// Only the job with this process's PID may be restarted automatically.
async fn managed_label(c: &impl Commands, uid: u32, current_pid: u32, desktop: bool)
    -> Result<Option<&'static str>, String>
{
    if desktop { return Ok(None); }
    for label in [LAUNCHD_LABEL, LEGACY_LAUNCHD_LABEL] {
        let reply = ctl(c, &["print".into(), target(uid, label)]).await?;
        if reply.success {
            if pid(&reply.stdout) == Some(current_pid) { return Ok(Some(label)); }
        } else if reply.stderr.lines().any(|line| line.trim().strip_prefix("Could not find domain for ").is_some()) {
            return Ok(None);
        } else if !missing(&reply) {
            return Err(format!("Cannot determine launchd install mode: {}", reply.stderr));
        }
    }
    Ok(None)
}

/// `installed_exe` is the path the update installed (canonical), which after a
/// bare → bundle migration differs from the running process's own path.
async fn validate_gateway(c: &impl Commands, home: &Path, label: &str, installed_exe: &Path)
    -> Result<(), String>
{
    let exe = installed_exe;
    let path = launchd_plist_path_in(home, label);
    let path = path.to_str().ok_or("Non-UTF-8 plist path")?;
    let reply = checked(c.run("/usr/bin/plutil", &[
        "-convert".into(), "json".into(), "-o".into(), "-".into(), "--".into(), path.into(),
    ]).await?)?;
    let plist: serde_json::Value = serde_json::from_str(&reply.stdout).map_err(|e| e.to_string())?;
    if plist["Label"].as_str() != Some(label)
        || plist["ProgramArguments"][0].as_str() != exe.to_str()
        || plist.get("Program").is_some_and(|v| v.as_str() != exe.to_str())
    {
        return Err("LaunchAgent plist does not name the installed gateway executable".into());
    }
    Ok(())
}

/// No shell, no downloaded path, no remote arguments. The label is allowlisted.
fn helper_plist(installed_exe: &Path, home: &Path, state: &Path, label: &str, gateway_pid: u32) -> Result<String, String> {
    if !allowed_label(label) || !installed_exe.is_absolute() || !home.is_absolute() || !state.is_absolute() || gateway_pid == 0 {
        return Err("Invalid update helper inputs".into());
    }
    let exe = xml_escape(installed_exe.to_str().ok_or("Non-UTF-8 installed executable path")?);
    let home_s = xml_escape(home.to_str().ok_or("Non-UTF-8 home path")?);
    let log = xml_escape(private_dir(home).join("relaunch.log").to_str().ok_or("Non-UTF-8 log path")?);
    let state_s = xml_escape(state.to_str().ok_or("Non-UTF-8 state path")?);
    let helper = helper_label(label);
    Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{helper}</string>
<key>ProgramArguments</key><array><string>{exe}</string><string>service</string><string>restart</string><string>--from-updater</string><string>{label}</string></array>
<key>RunAtLoad</key><true/>
<key>LaunchOnlyOnce</key><true/>
<key>KeepAlive</key><false/>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home_s}</string><key>DUDUCLAW_HOME</key><string>{state_s}</string><key>{PID_ENV}</key><string>{gateway_pid}</string></dict>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#))
}

#[cfg(target_os = "macos")]
fn write_helper(home: &Path, label: &str, content: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::io::Write;
    let dir = private_dir(home);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {},
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(e) => return Err(e.to_string()),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o077 != 0 {
        return Err("Update helper directory must be owned by this user with mode 0700".into());
    }
    let tmp = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, helper_path(home, label))?;
        std::fs::File::open(&dir)?.sync_all()
    })();
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result.map_err(|e| e.to_string())
}

/// Bounded unload + bootstrap; never kickstart a job with stale launch constraints.
async fn reregister(c: &impl Commands, uid: u32, label: &str, plist: &Path) -> Result<(), String> {
    let job = target(uid, label);
    let reply = ctl(c, &["bootout".into(), job.clone()]).await?;
    if !reply.success && !missing(&reply) { return Err(reply.stderr); }
    for attempt in 0..POLLS {
        let reply = ctl(c, &["print".into(), job.clone()]).await?;
        if missing(&reply) { break; }
        checked(reply)?;
        if attempt + 1 == POLLS { return Err("Timed out waiting for gateway to unload".into()); }
        c.pause().await;
    }
    checked(ctl(c, &["bootstrap".into(), format!("gui/{uid}"),
        plist.to_str().ok_or("Non-UTF-8 plist path")?.into()]).await?)?;
    for _ in 0..POLLS {
        let reply = checked(ctl(c, &["print".into(), job.clone()]).await?)?;
        if field(&reply.stdout, "state = ") == Some("running") && pid(&reply.stdout).is_some() {
            return Ok(());
        }
        c.pause().await;
    }
    Err("LaunchAgent registered but did not reach running state".into())
}

fn claim_schedule(state: &std::sync::atomic::AtomicU8) -> Result<bool, String> {
    match state.compare_exchange(0, 1, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst) {
        Ok(_) => Ok(false),
        Err(2) => Ok(true),
        Err(_) => Err("Automatic relaunch was already attempted; no automatic retry".into()),
    }
}

/// Returns true when launchd owns relaunch; false preserves the caller's normal
/// foreground/systemd/desktop restart. Failure MUST NOT fall back to exec.
#[cfg(target_os = "macos")]
pub async fn schedule_if_managed(desktop: bool) -> Result<bool, String> {
    schedule_if_managed_for(desktop, &crate::platform::executable_path()).await
}

/// [`schedule_if_managed`] for an update that installed the gateway at a
/// different path than this process runs from (bare → `DuDuClaw Gateway.app`
/// migration). The LaunchAgent must already name `installed_exe`; the helper
/// job runs `installed_exe`, never the old binary.
#[cfg(target_os = "macos")]
pub async fn schedule_if_managed_for(desktop: bool, installed_exe: &Path) -> Result<bool, String> {
    static SCHEDULED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    let c = SystemCommands;
    let uid = unsafe { libc::getuid() };
    let Some(label) = managed_label(&c, uid, std::process::id(), desktop).await? else { return Ok(false); };
    if claim_schedule(&SCHEDULED)? { return Ok(true); }
    // One attempt per gateway lifetime, including failures, prevents restart storms.
    let home = dirs_home()?;
    let exe = installed_exe.to_path_buf();
    validate_gateway(&c, &home, label, &exe).await?;
    let helper = target(uid, &helper_label(label));
    let old = ctl(&c, &["print".into(), helper.clone()]).await?;
    if old.success {
        if pid(&old.stdout).is_some() { return Err("An update relaunch helper is already running".into()); }
        checked(ctl(&c, &["bootout".into(), helper]).await?)?;
    } else if !missing(&old) { return Err(old.stderr); }
    write_helper(&home, label, &helper_plist(&exe, &home, &crate::platform::duduclaw_home(), label, std::process::id())?)?;
    tracing::info!(label, "Updater: submitting independent launchd relaunch helper");
    let result = ctl(&c, &["bootstrap".into(), format!("gui/{uid}"),
        helper_path(&home, label).to_str().ok_or("Non-UTF-8 helper path")?.into()]).await;
    match result.and_then(checked) {
        Ok(_) => {
            SCHEDULED.store(2, std::sync::atomic::Ordering::SeqCst);
            Ok(true)
        },
        Err(e) => {
            let _ = std::fs::remove_file(helper_path(&home, label));
            Err(e)
        }
    }
}

#[cfg(target_os = "macos")]
fn dirs_home() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is missing")?;
    if !home.is_absolute() { return Err("HOME must be absolute".into()); }
    Ok(home)
}

/// Hidden CLI lane, admitted only in the one-shot job. It consumes its plist
/// before any gateway operation, so duplicate invocations cannot restart twice.
#[cfg(target_os = "macos")]
pub async fn run_helper(label: &str) -> Result<(), String> {
    if !allowed_label(label) || std::env::var("XPC_SERVICE_NAME").ok().as_deref() != Some(&helper_label(label)) {
        return Err("--from-updater requires the dedicated update LaunchAgent".into());
    }
    let c = SystemCommands;
    let uid = unsafe { libc::getuid() };
    let home = dirs_home()?;
    let job = checked(ctl(&c, &["print".into(), target(uid, &helper_label(label))]).await?)?;
    if pid(&job.stdout) != Some(std::process::id()) { return Err("Not the registered update helper process".into()); }
    let expected_pid = std::env::var(PID_ENV).ok().and_then(|v| v.parse::<u32>().ok())
        .filter(|p| *p != 0).ok_or("Missing gateway PID")?;
    std::fs::remove_file(helper_path(&home, label)).map_err(|e| format!("Cannot consume helper plist: {e}"))?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(90),
        restart_expected(&c, &home, label, &crate::platform::executable_path(), uid, expected_pid))
        .await.unwrap_or_else(|_| Err("Update relaunch exceeded its 90-second deadline".into()));
    match &result {
        Ok(()) => println!("Updater: launchd gateway re-registered and running"),
        Err(e) => eprintln!("{MANUAL}: {e}"),
    }
    // LaunchOnlyOnce + KeepAlive=false leaves an inert job, removed on the next
    // update. Never bootout ourselves: that would kill this helper mid-report.
    result
}

async fn restart_expected(c: &impl Commands, home: &Path, label: &str, exe: &Path, uid: u32, expected_pid: u32) -> Result<(), String> {
    validate_gateway(c, home, label, exe).await?;
    let gateway = checked(ctl(c, &["print".into(), target(uid, label)]).await?)?;
    if pid(&gateway.stdout) != Some(expected_pid) {
        return Err("Gateway PID changed; refusing to restart a different process".into());
    }
    println!("Updater: re-registering launchd gateway {label}");
    // The gateway handles SIGINT for its bounded flush/drain. bootout alone
    // sends SIGTERM and would bypass that path. KeepAlive may start a new PID
    // when it exits; we still re-register exactly once to refresh constraints.
    checked(ctl(c, &["kill".into(), "SIGINT".into(), target(uid, label)]).await?)?;
    let graceful = async {
        for _ in 0..275 {
            let reply = ctl(c, &["print".into(), target(uid, label)]).await?;
            if missing(&reply) { return Ok(()); }
            let reply = checked(reply)?;
            if pid(&reply.stdout) != Some(expected_pid) { return Ok(()); }
            c.pause().await;
        }
        Err("Gateway did not finish graceful shutdown; refusing forced bootout".to_string())
    };
    tokio::time::timeout(std::time::Duration::from_secs(55), graceful).await
        .map_err(|_| "Gateway shutdown exceeded 55 seconds; refusing forced bootout".to_string())??;
    reregister(c, uid, label, &launchd_plist_path_in(home, label)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Fake { replies: Mutex<std::collections::VecDeque<Reply>>, calls: Mutex<Vec<(String, Vec<String>)>> }
    impl Fake {
        fn new(replies: Vec<Reply>) -> Self { Self { replies: Mutex::new(replies.into()), calls: Mutex::new(vec![]) } }
    }
    #[async_trait::async_trait]
    impl Commands for Fake {
        async fn run(&self, program: &str, args: &[String]) -> Result<Reply, String> {
            self.calls.lock().unwrap().push((program.into(), args.to_vec()));
            Ok(self.replies.lock().unwrap().pop_front().expect("unexpected command"))
        }
        async fn pause(&self) {}
    }
    fn ok(text: &str) -> Reply { Reply { success: true, stdout: text.into(), stderr: String::new() } }
    fn absent() -> Reply { Reply { success: false, stdout: String::new(), stderr: "Could not find service \"test\" in domain for user gui: 501".into() } }
    #[test]
    #[cfg(target_os = "macos")]
    fn plist_uses_installed_exe_fixed_args_and_single_run() {
        let p = helper_plist(Path::new("/Users/a & b/bin/duduclaw"), Path::new("/Users/a & b"), Path::new("/tmp/isolated-state"), LAUNCHD_LABEL, 42).unwrap();
        assert!(p.contains("<string>/Users/a &amp; b/bin/duduclaw</string><string>service</string><string>restart</string><string>--from-updater</string><string>com.duduclaw.gateway</string>"));
        assert!(p.contains("<key>LaunchOnlyOnce</key><true/>"));
        assert!(p.contains("<key>KeepAlive</key><false/>"));
        assert!(!p.contains("/bin/sh"));
        assert!(p.contains("<key>DUDUCLAW_HOME</key><string>/tmp/isolated-state</string>"));
        assert!(helper_plist(Path::new("/bin/duduclaw"), Path::new("/tmp"), Path::new("/tmp/state"), "other-job", 42).is_err());
        assert!(helper_plist(Path::new("relative"), Path::new("/tmp"), Path::new("/tmp/state"), LAUNCHD_LABEL, 42).is_err());
    }
    #[tokio::test]
    async fn detection_requires_current_pid_and_excludes_desktop() {
        let c = Fake::new(vec![]);
        assert_eq!(managed_label(&c, 501, 42, true).await.unwrap(), None);
        assert!(c.calls.lock().unwrap().is_empty());
        let c = Fake::new(vec![ok("pid = 99"), absent()]);
        assert_eq!(managed_label(&c, 501, 42, false).await.unwrap(), None);
        let c = Fake::new(vec![absent(), ok("pid = 42")]);
        assert_eq!(managed_label(&c, 501, 42, false).await.unwrap(), Some(LEGACY_LAUNCHD_LABEL));
        let c = Fake::new(vec![ok("pid = 42")]);
        assert_eq!(managed_label(&c, 501, 42, false).await.unwrap(), Some(LAUNCHD_LABEL));
    }
    #[tokio::test]
    async fn detection_fails_closed_on_launchctl_error() {
        let c = Fake::new(vec![Reply { success: false, stdout: String::new(), stderr: "permission denied".into() }]);
        assert!(managed_label(&c, 501, 42, false).await.is_err());
    }
    #[tokio::test]
    async fn registration_uses_bootout_then_bootstrap_and_waits_for_running() {
        let c = Fake::new(vec![ok(""), ok("state = running"), absent(), ok(""), ok("state = waiting"), ok("state = running\npid = 43")]);
        reregister(&c, 501, LAUNCHD_LABEL, Path::new("/tmp/test.plist")).await.unwrap();
        let calls = c.calls.lock().unwrap();
        assert_eq!(calls[0].1, ["bootout", "gui/501/com.duduclaw.gateway"]);
        assert_eq!(calls[3].1, ["bootstrap", "gui/501", "/tmp/test.plist"]);
        assert_eq!(calls.len(), 6);
    }
    #[tokio::test]
    async fn registration_is_bounded_and_stops_on_failure() {
        let mut replies = vec![ok("")];
        replies.extend((0..POLLS).map(|_| ok("state = running\npid = 42")));
        let c = Fake::new(replies);
        assert!(reregister(&c, 501, LAUNCHD_LABEL, Path::new("/tmp/test.plist")).await.unwrap_err().contains("Timed out"));
        assert_eq!(c.calls.lock().unwrap().len(), POLLS + 1);
        let c = Fake::new(vec![ok(""), absent(), Reply { success: false, stdout: String::new(), stderr: "bootstrap failed".into() }]);
        assert!(reregister(&c, 501, LAUNCHD_LABEL, Path::new("/tmp/test.plist")).await.is_err());
        assert_eq!(c.calls.lock().unwrap().len(), 3);
    }
    #[tokio::test]
    async fn rejects_plist_for_a_different_executable() {
        let c = Fake::new(vec![ok(r#"{"Label":"com.duduclaw.gateway","ProgramArguments":["/tmp/download","run"]}"#)]);
        assert!(validate_gateway(&c, Path::new("/tmp/home"), LAUNCHD_LABEL, Path::new("/installed/duduclaw")).await.is_err());
    }
    #[tokio::test]
    async fn headless_foreground_preserves_normal_restart() {
        let c = Fake::new(vec![Reply { success: false, stdout: String::new(), stderr: "Could not find domain for user gui: 501".into() }]);
        assert_eq!(managed_label(&c, 501, 42, false).await.unwrap(), None);
    }
    #[tokio::test]
    async fn changed_pid_never_signals_or_boots_out_gateway() {
        let c = Fake::new(vec![ok(r#"{"Label":"com.duduclaw.gateway","ProgramArguments":["/installed/duduclaw","run"]}"#), ok("pid = 99")]);
        assert!(restart_expected(&c, Path::new("/tmp/home"), LAUNCHD_LABEL, Path::new("/installed/duduclaw"), 501, 42).await.is_err());
        assert_eq!(c.calls.lock().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn helper_flushes_gateway_before_reregistering() {
        let c = Fake::new(vec![ok(r#"{"Label":"com.duduclaw.gateway","ProgramArguments":["/installed/duduclaw","run"]}"#), ok("pid = 42"), ok(""), ok("pid = 42"), ok("pid = 43"), ok(""), absent(), ok(""), ok("state = running\npid = 44")]);
        restart_expected(&c, Path::new("/tmp/home"), LAUNCHD_LABEL, Path::new("/installed/duduclaw"), 501, 42).await.unwrap();
        let calls = c.calls.lock().unwrap();
        assert_eq!(calls[2].1, ["kill", "SIGINT", "gui/501/com.duduclaw.gateway"]);
        assert_eq!(calls[5].1, ["bootout", "gui/501/com.duduclaw.gateway"]);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn helper_plist_is_private_atomic_and_symlink_directory_is_refused() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let home = tempfile::tempdir().unwrap();
        write_helper(home.path(), LAUNCHD_LABEL, "one").unwrap();
        let path = helper_path(home.path(), LAUNCHD_LABEL);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(private_dir(home.path())).unwrap().mode() & 0o777, 0o700);
        write_helper(home.path(), LAUNCHD_LABEL, "two").unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "two");
        let other = tempfile::tempdir().unwrap();
        symlink(private_dir(home.path()), private_dir(other.path())).unwrap();
        assert!(write_helper(other.path(), LAUNCHD_LABEL, "three").is_err());
    }

    #[test]
    fn repeated_failed_attempts_are_not_reported_as_scheduled() {
        use std::sync::atomic::{AtomicU8, Ordering};
        let state = AtomicU8::new(0);
        assert!(!claim_schedule(&state).unwrap());
        assert!(claim_schedule(&state).is_err());
        state.store(2, Ordering::SeqCst);
        assert!(claim_schedule(&state).unwrap());
    }
    #[test]
    fn job_pid_ignores_nested_environment_fields() {
        assert_eq!(pid("\tenvironment = {\n\t\tpid = 99\n\t}\n\tpid = 42"), Some(42));
        assert_eq!(pid("\tenvironment = {\n\t\tpid = 99\n\t}"), None);
    }
    #[tokio::test]
    async fn migrated_bundle_exe_is_validated_and_launched_by_the_helper() {
        let installed = Path::new("/Users/k/Applications/DuDuClaw Gateway.app/Contents/MacOS/duduclaw");
        let c = Fake::new(vec![ok(r#"{"Label":"com.duduclaw.gateway","ProgramArguments":["/Users/k/Applications/DuDuClaw Gateway.app/Contents/MacOS/duduclaw","run"]}"#)]);
        validate_gateway(&c, Path::new("/tmp/home"), LAUNCHD_LABEL, installed).await.unwrap();
        // A plist still naming the old bare binary does not authorize the helper.
        let c = Fake::new(vec![ok(r#"{"Label":"com.duduclaw.gateway","ProgramArguments":["/Users/k/.duduclaw/bin/duduclaw","run"]}"#)]);
        assert!(validate_gateway(&c, Path::new("/tmp/home"), LAUNCHD_LABEL, installed).await.is_err());
        let p = helper_plist(installed, Path::new("/Users/k"), Path::new("/Users/k/.duduclaw"), LAUNCHD_LABEL, 42).unwrap();
        assert!(p.contains("<string>/Users/k/Applications/DuDuClaw Gateway.app/Contents/MacOS/duduclaw</string><string>service</string>"));
    }

    #[tokio::test]
    async fn program_override_cannot_replace_installed_executable() {
        let c = Fake::new(vec![ok(r#"{"Label":"com.duduclaw.gateway","Program":"/tmp/other","ProgramArguments":["/installed/duduclaw","run"]}"#)]);
        assert!(validate_gateway(&c, Path::new("/tmp/home"), LAUNCHD_LABEL, Path::new("/installed/duduclaw")).await.is_err());
    }
    #[tokio::test]
    async fn stuck_gateway_is_not_forcibly_booted_out() {
        let mut replies = vec![ok(r#"{"Label":"com.duduclaw.gateway","ProgramArguments":["/installed/duduclaw","run"]}"#), ok("pid = 42"), ok("")];
        replies.extend((0..275).map(|_| ok("pid = 42")));
        let c = Fake::new(replies);
        assert!(restart_expected(&c, Path::new("/tmp/home"), LAUNCHD_LABEL, Path::new("/installed/duduclaw"), 501, 42).await.is_err());
        assert!(!c.calls.lock().unwrap().iter().any(|(_, args)| args[0] == "bootout"));
    }

}
