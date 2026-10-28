//! Fixed system-browser discovery and owned-profile launch. No configurable
//! executable, arbitrary arguments, TCP debugging, or sandbox weakening.
use super::policy::dedicated_profile;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

pub(crate) const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
pub(crate) const EDGE: &str = "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge";
// ChromiumDash Stable Mac milestone 156, checked 2026-10-24. This build's
// minimum is explicitly versioned; releases must refresh it with native tests.
pub(crate) const MIN_BROWSER_MAJOR: u32 = 154;
pub(crate) fn version_supported(product: &str) -> bool {
    let Some(version) = product
        .strip_prefix("Chrome/")
        .or_else(|| product.strip_prefix("Edg/"))
    else {
        return false;
    };
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= MIN_BROWSER_MAJOR)
}

pub(crate) fn browser() -> Result<PathBuf, String> {
    if !super::policy::platform_supported() {
        return Err("local_browser_platform_unsupported".into());
    }
    [CHROME, EDGE]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| "local_browser_missing".into())
}

pub(crate) fn arguments(
    profile: &Path,
    proxy: std::net::SocketAddr,
    width: u32,
    height: u32,
) -> Vec<String> {
    vec![
        "--remote-debugging-pipe".into(),
        format!("--user-data-dir={}", profile.display()),
        "--profile-directory=Default".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--new-window".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-sync".into(),
        "--disable-extensions".into(),
        "--disable-component-extensions-with-background-pages".into(),
        "--disable-default-apps".into(),
        "--disable-quic".into(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
        format!("--proxy-server=http://{proxy}"),
        "--proxy-bypass-list=<-loopback>".into(),
        "--disable-features=MediaRouter,DialMediaRouteProvider,AutofillServerCommunication".into(),
        format!(
            "--window-size={},{}",
            width.clamp(320, 1920),
            height.clamp(240, 1200)
        ),
        "about:blank".into(),
    ]
}

#[cfg(unix)]
pub(crate) async fn spawn(
    home: &Path,
    agent: &str,
    proxy: std::net::SocketAddr,
    width: u32,
    height: u32,
) -> Result<(tokio::process::Child, tokio::net::UnixStream), String> {
    use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
    let executable = browser()?;
    let profile = dedicated_profile(home, agent)?;
    std::fs::create_dir_all(&profile).map_err(|_| "local profile unavailable")?;
    // Re-check after creation; a linked ancestor is never accepted by launch.
    let profile = dedicated_profile(home, agent)?;
    for directory in [
        profile.clone(),
        profile.parent().ok_or("invalid profile")?.to_path_buf(),
        home.join("local_browser"),
    ] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "local profile permissions unavailable")?;
    }
    let (parent, child) =
        std::os::unix::net::UnixStream::pair().map_err(|_| "browser pipe unavailable")?;
    parent
        .set_nonblocking(true)
        .map_err(|_| "browser pipe unavailable")?;
    // Duplicate above 4 before pre_exec so a collision with inherited stdin or
    // another pipe cannot destroy the descriptor used by the second dup2.
    let fd = unsafe { libc::fcntl(child.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
    if fd < 0 {
        return Err("browser pipe unavailable".into());
    }
    use std::os::fd::FromRawFd;
    let inherited = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let mut command = tokio::process::Command::new(executable);
    #[cfg(all(test, target_os = "macos"))]
    if super::native_fixture::active() {
        command.arg("--use-mock-keychain");
    }
    command
        .args(arguments(&profile, proxy, width, height))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.env_clear();
    command.env("HOME", profile.parent().ok_or("invalid browser home")?);
    command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    command.env(
        "BREAKPAD_DUMP_LOCATION",
        profile
            .parent()
            .ok_or("invalid browser home")?
            .join("crashes"),
    );
    command.process_group(0);
    // Every environment value is host-owned; employee/model arguments are
    // never forwarded into a native browser process.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::dup2(fd, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(fd);
            Ok(())
        });
    }
    let child_process = command.spawn().map_err(|_| "browser launch failed")?;
    drop(inherited);
    drop(child);
    let pipe = tokio::net::UnixStream::from_std(parent).map_err(|_| "browser pipe unavailable")?;
    Ok((child_process, pipe))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_retains_sandbox_and_never_opens_tcp_debugging_or_mock_keychain() {
        let args = arguments(
            Path::new("/owned/profile"),
            "127.0.0.1:41234".parse().unwrap(),
            1000,
            700,
        );
        assert!(args.iter().any(|a| a == "--remote-debugging-pipe"));
        assert!(args.iter().any(|a| a == "--profile-directory=Default"));
        assert!(args.iter().any(|a| a == "--proxy-bypass-list=<-loopback>"));
        assert!(args.iter().any(|a| a == "--disable-quic"));
        for forbidden in [
            "--no-sandbox",
            "--disable-setuid-sandbox",
            "--remote-debugging-port",
            "--use-mock-keychain",
            "--password-store=basic",
            "--ignore-certificate-errors",
        ] {
            assert!(
                !args.iter().any(|a| a.starts_with(forbidden)),
                "{forbidden}"
            );
        }
    }
    #[test]
    fn browser_version_must_be_reported_by_the_pipe_and_meet_the_build_floor() {
        assert!(version_supported("Chrome/156.0.8078.47"));
        assert!(version_supported("Edg/154.0.0.0"));
        for invalid in [
            "Chrome/153.99",
            "Chromium/156.1",
            "Chrome/bad",
            "156.0.0",
            "",
        ] {
            assert!(!version_supported(invalid));
        }
    }
}
