//! v1.80 in-process upgrade for single-binary protected installs (stable floor
//! >= 1.80, no updater-managed generation). Kept until the isolation bootstrap
//! is installable (planned v1.82.0). `download_protected_release` fetches the
//! fixed official release URL and verifies checksum, minisign signature and the
//! embedded version without executing anything; this module owns only the
//! root-only staging file, the atomic swap and the service contract refresh.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route { Updater, InPlace, Migration }

/// Decided only from installed state, never from the outcome of a request.
pub(super) fn route(managed: bool, floor: Option<&str>) -> Route {
    if managed { return Route::Updater; }
    match floor.map(semver::Version::parse) {
        Some(Ok(floor)) if floor.pre.is_empty() && floor.build.is_empty()
            && (floor.major, floor.minor) >= (1, 80) => Route::InPlace,
        _ => Route::Migration,
    }
}

fn service_active() -> Result<bool> {
    // Unknown systemctl errors are never mistaken for an inactive service.
    match command("systemctl", &["is-active", "--quiet", UNIT])?.status.code() {
        Some(0) => Ok(true),
        Some(3 | 4) => Ok(false),
        _ => Err(refused("cannot determine system service state")),
    }
}

/// Root-only staging file beside the installed binary, so the swap is one rename.
fn staging_file() -> Result<tempfile::NamedTempFile> {
    let binary = Path::new(BINARY);
    parents(binary)?;
    match fs::symlink_metadata(binary) {
        Ok(_) => trusted(binary, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            return Err(refused("找不到已安裝的受保護服務程式；請先執行 sudo duduclaw service install --protected-linux。")),
        Err(error) => return Err(error.into()),
    }
    let parent = binary.parent().ok_or_else(|| refused("missing parent"))?;
    let staged = tempfile::NamedTempFile::new_in(parent)?;
    if unsafe { libc::fchown(staged.as_raw_fd(), 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    staged.as_file().set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(staged)
}

/// The unit and launcher written below come from the CLI that runs this, so
/// an older (still verified) CLI must not install them over a newer floor.
/// Compared with the recorded floor, not the pending version, so the newer
/// binary left by an interrupted swap can still finish the upgrade.
pub(super) fn cli_covers_floor(cli: &str, floor: Option<&str>) -> bool {
    match (semver::Version::parse(cli), floor.map(semver::Version::parse)) {
        (Ok(cli), Some(Ok(floor))) => cli >= floor,
        _ => false,
    }
}

/// Caller holds the version lock and has already run the runtime preflight.
pub(super) async fn run(requested: Option<&str>) -> Result<()> {
    let cli = duduclaw_gateway::updater::current_version();
    let floor = read_version_file(VERSION_FLOOR)?;
    if !cli_covers_floor(cli, floor.as_deref()) {
        return Err(refused(format!("目前執行的 duduclaw（{cli}）比已安裝的版本下限（{}）舊，會寫入較舊的服務單元與啟動器，未變更安裝。請改用已安裝的程式執行：sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux",
            floor.as_deref().unwrap_or("未知"))));
    }
    // Refuse a known-bad target before downloading anything.
    if let Some(requested) = requested { check_floor(requested, true, false)?; }
    let mut staged = staging_file()?;
    let (target, _, _) = duduclaw_gateway::updater::download_protected_release(requested, staged.as_file_mut())
        .await.map_err(refused)?;
    if requested.is_some_and(|requested| requested != target) {
        return Err(refused("下載的版本與要求不符，未變更安裝。"));
    }
    check_floor(&target, true, false)?;
    let was_active = service_active()?;
    invalidate_recovery_bound()?;
    prepare_version(&target)?;
    staged.as_file().set_permissions(fs::Permissions::from_mode(0o755))?;
    staged.as_file().sync_all()?;
    staged.persist(BINARY).map_err(|error| error.error)?;
    fs::File::open(Path::new(BINARY).parent().ok_or_else(|| refused("missing parent"))?)?.sync_all()?;
    if update_text_if_different(LAUNCHER, 0o755, LAUNCHER_TEXT)? { println!("已更新受保護啟動器。"); }
    if update_text_if_different(UNIT_PATH, 0o644, &unit_text())? { println!("已更新服務單元。"); }
    record_floor(&target)?;
    reload_and_certify_recovery()?;
    if was_active {
        checked("systemctl", &["restart", UNIT])?;
        println!("已升級受保護服務至 {target}，並已重新啟動 {UNIT}。");
    } else {
        println!("已升級受保護服務至 {target}。服務目前未執行；啟動：sudo systemctl start {UNIT}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cli_covers_floor, route, Route};

    #[test]
    fn service_protected_upgrade_cli_must_cover_recorded_floor() {
        assert!(cli_covers_floor("1.81.0", Some("1.81.0")));
        assert!(cli_covers_floor("1.81.2", Some("1.81.0")));
        assert!(cli_covers_floor("2.0.0", Some("1.80.1")));
        // An older verified CLI would write older unit/launcher templates.
        assert!(!cli_covers_floor("1.80.1", Some("1.81.0")));
        assert!(!cli_covers_floor("1.81.0-rc.1", Some("1.81.0")));
        // Unknown or malformed values never pass.
        for floor in [None, Some("bad"), Some(""), Some(" 1.80.1"), Some("1.80.1\n")] {
            assert!(!cli_covers_floor("1.81.0", floor), "{floor:?}");
        }
        assert!(!cli_covers_floor("bad", Some("1.80.1")));
    }

    #[test]
    fn service_protected_upgrade_route_follows_installed_state_only() {
        // An updater-managed generation never reaches the in-process path.
        for floor in [None, Some("1.79.3"), Some("1.80.1"), Some("bad")] {
            assert_eq!(route(true, floor), Route::Updater);
        }
        for floor in ["1.80.0", "1.80.1", "1.81.0", "2.0.0"] {
            assert_eq!(route(false, Some(floor)), Route::InPlace);
        }
        // Unknown, legacy, non-stable or malformed floors need G's migration.
        for floor in [None, Some("1.79.3"), Some("1.80.1\n"), Some(" 1.80.1"), Some("1.80.0-rc.1"),
            Some("1.80.0+build"), Some("bad"), Some("")] {
            assert_eq!(route(false, floor), Route::Migration, "{floor:?}");
        }
    }
}
