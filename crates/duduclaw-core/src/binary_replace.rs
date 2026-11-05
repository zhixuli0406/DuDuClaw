//! Replacement of an already-verified, same-directory executable.
//! Integrity/version checks belong to the caller. Callers must serialize updates.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
enum Step { BackupReady, BeforeReplace, AfterReplace, BeforeRollback, AfterRollback }

/// Flush staging and preserve the old executable before changing the install path.
/// Unix replacement and rollback each use one rename over the destination.
/// Windows retains a rename-aside fallback for mapped executables; that fallback
/// recovers synchronous failures, but cannot promise crash atomicity between moves.
pub fn replace_verified_binary(staged: &Path, target: &Path) -> Result<(), String> {
    replace_with_hook(staged, target, &mut |_| Ok(()))
}

fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    // Fixed Windows names let the next update reclaim mapped old images
    // after the previous process exits, without scanning the install directory.
    #[cfg(windows)]
    name.push(format!(".{suffix}"));
    #[cfg(not(windows))]
    name.push(format!(".{}.{suffix}", uuid::Uuid::new_v4()));
    PathBuf::from(name)
}

#[cfg(windows)]
fn remove_previous(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn sync_directory(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    { File::open(dir)?.sync_all() }
    #[cfg(not(unix))]
    { let _ = dir; Ok(()) }
}

fn backup_binary(target: &Path, backup: &Path) -> io::Result<()> {
    if fs::hard_link(target, backup).is_ok() {
        #[cfg(unix)]
        File::open(backup)?.sync_all()?;
        return Ok(());
    }
    copy_binary(target, backup)
}

fn copy_binary(target: &Path, backup: &Path) -> io::Result<()> {
    // create_new prevents a stale file or symlink from being overwritten.
    let mut source = File::open(target)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(backup)?;
    io::copy(&mut source, &mut output)?;
    output.set_permissions(source.metadata()?.permissions())?;
    output.sync_all()
}

fn replace_with_hook(
    staged: &Path,
    target: &Path,
    hook: &mut impl FnMut(Step) -> io::Result<()>,
) -> Result<(), String> {
    replace_with_ops(staged, target, hook, &mut platform_replace)
}

fn replace_with_ops(
    staged: &Path,
    target: &Path,
    hook: &mut impl FnMut(Step) -> io::Result<()>,
    replace: &mut impl FnMut(&Path, &Path, bool) -> io::Result<()>,
) -> Result<(), String> {
    let dir = target.parent().ok_or("Binary has no parent directory")?;
    let prepare = || -> io::Result<()> {
        if staged == target || staged.file_name() == target.file_name()
            || fs::canonicalize(staged.parent().unwrap_or(Path::new(".")))? != fs::canonicalize(dir)?
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "staging must be a different file in the install directory"));
        }
        for path in [staged, target] {
            if !fs::symlink_metadata(path)?.file_type().is_file() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "binary must be a regular file"));
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if fs::metadata(staged)?.ino() == fs::metadata(target)?.ino() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "staging aliases the installed binary"));
            }
            fs::set_permissions(staged, fs::Permissions::from_mode(0o755))?;
        }
        OpenOptions::new().read(true).write(true).open(staged)?.sync_all()
    };
    prepare().map_err(|e| format!("Failed to prepare binary replacement: {e}"))?;
    let backup = sibling(target, "bak");
    #[cfg(windows)]
    for suffix in ["bak", "rollback", "replace-backup", "running-old", "rollback-replace-backup", "rollback-running-old"] {
        remove_previous(&sibling(target, suffix))
            .map_err(|e| format!("Cannot reclaim previous binary backup: {e}"))?;
    }
    if let Err(e) = backup_binary(target, &backup).and_then(|()| sync_directory(dir)) {
        let _ = fs::remove_file(&backup);
        return Err(format!("Failed to preserve current binary: {e}"));
    }
    if let Err(e) = hook(Step::BackupReady).and_then(|()| hook(Step::BeforeReplace)) {
        let _ = fs::remove_file(&backup);
        return Err(format!("Binary replacement refused before install: {e}"));
    }
    let install = replace(staged, target, false)
        .and_then(|()| hook(Step::AfterReplace))
        .and_then(|()| sync_directory(dir));
    if let Err(e) = install {
        // Also covers partial Windows ReplaceFileW failures, which may have
        // moved the old file to the API's backup name before reporting failure.
        let rollback_staged = sibling(target, "rollback");
        let rollback = hook(Step::BeforeRollback)
            .and_then(|()| backup_binary(&backup, &rollback_staged))
            .and_then(|()| replace(&rollback_staged, target, true))
            .and_then(|()| hook(Step::AfterRollback))
            .and_then(|()| sync_directory(dir));
        let _ = fs::remove_file(&rollback_staged);
        return match rollback {
            Ok(()) => {
                // rename of two hard links to the same inode is a no-op.
                let _ = fs::remove_file(&backup);
                let _ = sync_directory(dir);
                Err(format!("Failed to install binary (rolled back successfully): {e}"))
            }
            Err(rb) => Err(format!("Failed to install binary: {e}; rollback failed: {rb}. Recovery binary: {}", backup.display())),
        };
    }
    // Once the directory sync succeeded, cleanup cannot invalidate the install.
    let _ = fs::remove_file(&backup);
    let _ = sync_directory(dir);
    Ok(())
}

#[cfg(unix)]
fn platform_replace(staged: &Path, target: &Path, _rollback: bool) -> io::Result<()> {
    fs::rename(staged, target)
}

#[cfg(windows)]
fn platform_replace(staged: &Path, target: &Path, rollback: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"));
        }
        value.push(0);
        Ok(value)
    }
    fn move_file(from: &Path, to: &Path, replace: bool) -> io::Result<()> {
        let from = wide(from)?;
        let to = wide(to)?;
        let flags = MOVEFILE_WRITE_THROUGH | if replace { MOVEFILE_REPLACE_EXISTING } else { 0 };
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
            Err(io::Error::last_os_error())
        } else { Ok(()) }
    }

    windows_replace_with(staged, target, rollback, &mut |staged, target, backup| {
        let target_w = wide(target)?;
        let staged_w = wide(staged)?;
        let backup_w = wide(backup)?;
        let replaced = unsafe {
            ReplaceFileW(target_w.as_ptr(), staged_w.as_ptr(), backup_w.as_ptr(), 0,
                std::ptr::null(), std::ptr::null())
        };
        if replaced == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }, &mut move_file)
}

// The state machine is platform-neutral so partial Windows API failures and
// the running-image recovery can be injected on any test host.
#[cfg(any(windows, test))]
fn windows_replace_with(
    staged: &Path,
    target: &Path,
    rollback: bool,
    replace_file: &mut impl FnMut(&Path, &Path, &Path) -> io::Result<()>,
    move_file: &mut impl FnMut(&Path, &Path, bool) -> io::Result<()>,
) -> io::Result<()> {
    // Distinct names keep rollback from touching the old image retained by a
    // failed install. Previous-run cleanup happens only once, before backup.
    let api_backup = sibling(target, if rollback { "rollback-replace-backup" } else { "replace-backup" });
    let aside = sibling(target, if rollback { "rollback-running-old" } else { "running-old" });
    let replace_error = match replace_file(staged, target, &api_backup) {
        Ok(()) => {
            // A mapped old image may only be removable on the next update.
            let _ = fs::remove_file(&api_backup);
            return Ok(());
        }
        Err(e) => e,
    };
    // 1177 means ReplaceFileW already renamed the old file to its backup.
    // Restore it before attempting anything else; never discard that backup.
    if replace_error.raw_os_error() == Some(1177) {
        if let Err(rb) = move_file(&api_backup, target, true) {
            return Err(io::Error::other(format!(
                "ReplaceFileW failed: {replace_error}; restore failed: {rb}; old binary: {}", api_backup.display())));
        }
        return Err(replace_error);
    }
    let move_error = match move_file(staged, target, true) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    // Rollback must not introduce a rename-aside gap. If a new image has
    // already been mapped, retain recovery copies and report the refusal.
    // Only install sharing/access failures justify the running-image fallback.
    if rollback || !matches!(replace_error.raw_os_error(), Some(5 | 32 | 1175))
        || !matches!(move_error.raw_os_error(), Some(5 | 32))
    {
        return Err(move_error);
    }
    move_file(target, &aside, false)?;
    if let Err(e) = move_file(staged, target, false) {
        if let Err(rb) = move_file(&aside, target, true) {
            return Err(io::Error::other(
                format!("install failed: {e}; restore failed: {rb}; old binary: {}", aside.display())));
        }
        return Err(e);
    }
    let _ = fs::remove_file(&aside);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn platform_replace(_: &Path, _: &Path, _: bool) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "binary replacement unsupported"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("duduclaw.exe");
        let staged = dir.path().join("staged.exe");
        fs::write(&target, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();
        (dir, target, staged)
    }

    #[test]
    fn binary_replace_success() {
        let (dir, target, staged) = fixture();
        replace_verified_binary(&staged, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!staged.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(target).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn binary_replace_preinstall_failures_keep_target() {
        for step in [Step::BackupReady, Step::BeforeReplace] {
            let (_dir, target, staged) = fixture();
            let error = replace_with_hook(&staged, &target, &mut |at| {
                assert_eq!(fs::read(&target).unwrap(), b"old");
                if at == step { Err(io::Error::other("injected")) } else { Ok(()) }
            }).unwrap_err();
            assert!(error.contains("before install"));
            assert_eq!(fs::read(target).unwrap(), b"old");
            assert_eq!(fs::read(staged).unwrap(), b"new");
        }
    }

    #[test]
    fn binary_replace_failed_rename_keeps_old() {
        let (_dir, target, staged) = fixture();
        let error = replace_with_hook(&staged, &target, &mut |at| {
            if at == Step::BeforeReplace { fs::remove_file(&staged)?; }
            Ok(())
        }).unwrap_err();
        assert!(error.contains("rolled back successfully"));
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn binary_replace_postinstall_failure_rolls_back() {
        let (_dir, target, staged) = fixture();
        let error = replace_with_hook(&staged, &target, &mut |at| {
            let expected: &[u8] = if at == Step::AfterReplace || at == Step::BeforeRollback { b"new" } else { b"old" };
            assert_eq!(fs::read(&target).unwrap(), expected);
            if at == Step::AfterReplace { Err(io::Error::other("injected sync failure")) } else { Ok(()) }
        }).unwrap_err();
        assert!(error.contains("rolled back successfully"));
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn binary_replace_rollback_failure_preserves_recovery_copy() {
        let (dir, target, staged) = fixture();
        let error = replace_with_hook(&staged, &target, &mut |at| {
            if at == Step::AfterReplace || at == Step::BeforeRollback {
                Err(io::Error::other("injected"))
            } else { Ok(()) }
        }).unwrap_err();
        assert!(error.contains("rollback failed"));
        assert_eq!(fs::read(target).unwrap(), b"new");
        let backup = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|ext| ext == "bak")).unwrap();
        assert_eq!(fs::read(backup).unwrap(), b"old");
    }

    #[test]
    fn binary_replace_rejects_other_directory_and_alias() {
        let (_dir, target, _staged) = fixture();
        let other = tempfile::tempdir().unwrap();
        let external = other.path().join("new");
        fs::write(&external, b"new").unwrap();
        assert!(replace_verified_binary(&external, &target).is_err());
        assert!(replace_verified_binary(&target, &target).is_err());
        #[cfg(unix)]
        {
            fs::remove_file(&_staged).unwrap();
            fs::hard_link(&target, &_staged).unwrap();
            assert!(replace_verified_binary(&_staged, &target).is_err());
        }
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn binary_replace_rollback_sync_failure_keeps_recovery_path() {
        let (dir, target, staged) = fixture();
        let error = replace_with_hook(&staged, &target, &mut |at| {
            if at == Step::AfterReplace || at == Step::AfterRollback {
                Err(io::Error::other("injected sync failure"))
            } else { Ok(()) }
        }).unwrap_err();
        assert_eq!(fs::read(target).unwrap(), b"old");
        let backup = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|ext| ext == "bak")).unwrap();
        assert!(error.contains(&backup.display().to_string()));
        assert_eq!(fs::read(backup).unwrap(), b"old");
    }

    #[test]
    fn binary_replace_copy_backup_is_exclusive_and_complete() {
        let (dir, target, _staged) = fixture();
        let backup = dir.path().join("copy");
        copy_binary(&target, &backup).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"old");
        assert!(copy_binary(&target, &backup).is_err());
        fs::write(&target, b"changed").unwrap();
        assert_eq!(fs::read(backup).unwrap(), b"old");
    }

    #[test]
    fn binary_replace_windows_partial_replace_restores_backup() {
        let (dir, target, staged) = fixture();
        let error = windows_replace_with(&staged, &target, false, &mut |_, target, backup| {
            fs::rename(target, backup)?;
            Err(io::Error::from_raw_os_error(1177))
        }, &mut |from, to, replace| {
            assert!(replace);
            fs::rename(from, to)
        }).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(1177));
        assert_eq!(fs::read(target).unwrap(), b"old");
        assert_eq!(fs::read(staged).unwrap(), b"new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn binary_replace_windows_move_fallback() {
        let (_dir, target, staged) = fixture();
        windows_replace_with(&staged, &target, false, &mut |_, _, _| {
            Err(io::Error::from_raw_os_error(1176))
        }, &mut |from, to, replace| {
            assert!(replace);
            fs::rename(from, to)
        }).unwrap();
        assert_eq!(fs::read(target).unwrap(), b"new");
    }

    #[test]
    fn binary_replace_windows_running_fallback_restores_on_failure() {
        for fail_install in [false, true] {
            let (_dir, target, staged) = fixture();
            let mut calls = 0;
            let result = windows_replace_with(&staged, &target, false, &mut |_, _, _| {
                Err(io::Error::from_raw_os_error(32))
            }, &mut |from, to, replace| {
                calls += 1;
                match calls {
                    1 => { assert!(replace); Err(io::Error::from_raw_os_error(32)) }
                    2 => { assert!(!replace); fs::rename(from, to) }
                    3 if fail_install => Err(io::Error::from_raw_os_error(112)),
                    _ => fs::rename(from, to),
                }
            });
            if fail_install {
                assert!(result.is_err());
                assert_eq!(calls, 4);
                assert_eq!(fs::read(target).unwrap(), b"old");
            } else {
                result.unwrap();
                assert_eq!(calls, 3);
                assert_eq!(fs::read(target).unwrap(), b"new");
            }
        }
    }

    #[test]
    fn binary_replace_windows_failed_restore_keeps_old_image() {
        for partial_replace in [false, true] {
            let (dir, target, staged) = fixture();
            let mut calls = 0;
            let error = windows_replace_with(&staged, &target, false, &mut |_, target, backup| {
                if partial_replace {
                    fs::rename(target, backup)?;
                    Err(io::Error::from_raw_os_error(1177))
                } else { Err(io::Error::from_raw_os_error(32)) }
            }, &mut |from, to, _| {
                calls += 1;
                if !partial_replace && calls == 2 { fs::rename(from, to) }
                else { Err(io::Error::from_raw_os_error(32)) }
            }).unwrap_err();
            assert!(error.to_string().contains("restore failed"));
            let suffix = if partial_replace { "replace-backup" } else { "running-old" };
            let backup = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|ext| ext == suffix)).unwrap();
            assert_eq!(fs::read(backup).unwrap(), b"old");
        }
    }

    #[test]
    fn binary_replace_windows_other_errors_do_not_rename_target() {
        let (_dir, target, staged) = fixture();
        let mut calls = 0;
        assert!(windows_replace_with(&staged, &target, false, &mut |_, _, _| {
            Err(io::Error::from_raw_os_error(32))
        }, &mut |_, _, _| {
            calls += 1;
            Err(io::Error::from_raw_os_error(112))
        }).is_err());
        assert_eq!(calls, 1);
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn binary_replace_windows_outer_rollback_keeps_retained_image() {
        for partial_replace in [false, true] {
            let (_dir, target, staged) = fixture();
            let retained = std::cell::RefCell::new(None::<PathBuf>);
            let error = replace_with_ops(&staged, &target, &mut |_| Ok(()), &mut |staged, target, rollback| {
                if rollback {
                    let path = retained.borrow().clone().unwrap();
                    assert_eq!(fs::read(&path).unwrap(), b"old");
                    let result = windows_replace_with(staged, target, true, &mut |_, _, backup| {
                        assert_eq!(backup.extension().unwrap(), "rollback-replace-backup");
                        Err(io::Error::from_raw_os_error(2))
                    }, &mut |from, to, _| fs::rename(from, to));
                    assert_eq!(fs::read(path).unwrap(), b"old");
                    return result;
                }
                let mut moves = 0;
                windows_replace_with(staged, target, false, &mut |_, target, backup| {
                    if partial_replace {
                        fs::rename(target, backup)?;
                        *retained.borrow_mut() = Some(backup.to_path_buf());
                        Err(io::Error::from_raw_os_error(1177))
                    } else { Err(io::Error::from_raw_os_error(32)) }
                }, &mut |from, to, _| {
                    moves += 1;
                    if !partial_replace && moves == 2 {
                        fs::rename(from, to)?;
                        *retained.borrow_mut() = Some(to.to_path_buf());
                        Ok(())
                    } else { Err(io::Error::from_raw_os_error(32)) }
                })
            }).unwrap_err();
            assert!(error.contains("rolled back successfully"));
            assert_eq!(fs::read(target).unwrap(), b"old");
        }
    }

    #[test]
    fn binary_replace_windows_rollback_never_renames_target_aside() {
        let (_dir, target, staged) = fixture();
        let mut moves = 0;
        assert!(windows_replace_with(&staged, &target, true, &mut |_, _, _| {
            Err(io::Error::from_raw_os_error(32))
        }, &mut |_, _, replace| {
            moves += 1;
            assert!(replace);
            Err(io::Error::from_raw_os_error(32))
        }).is_err());
        assert_eq!(moves, 1);
        assert_eq!(fs::read(target).unwrap(), b"old");
    }
}
