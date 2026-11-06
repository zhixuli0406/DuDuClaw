use super::*;
use std::path::Path;

#[test]
fn creates_reads_lists_and_removes_archives() {
    let temp = private_test_home().unwrap();
    let path = temp.path().join("archives/nested");
    assert_eq!(SafeDir::open_nofollow(&path, false).err().unwrap().kind(), io::ErrorKind::NotFound);
    let dir = SafeDir::open_nofollow(&path, true).unwrap();
    assert!(dir.metadata().unwrap().is_dir());
    assert!(dir.open_read_checked("missing", true).unwrap().is_none());
    assert!(dir.create_file_new("交付.txt", b"original").unwrap());
    assert!(!dir.create_file_new("交付.txt", b"replacement").unwrap());
    assert_eq!(dir.read_file("交付.txt").unwrap().as_deref(), Some("original"));
    assert!(dir.read_bytes_checked("交付.txt", 3, true).is_err());
    let entries = dir.list().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].name.as_str(), entries[0].kind, entries[0].nlink), ("交付.txt", EntryKind::File, 1));
    dir.write_file_atomic("交付.txt", "updated").unwrap();
    assert_eq!(dir.read_file("交付.txt").unwrap().as_deref(), Some("updated"));
    dir.remove_file("交付.txt").unwrap();
    assert!(dir.list().unwrap().is_empty());
    assert_eq!(dir.remove_file("交付.txt").unwrap_err().kind(), io::ErrorKind::NotFound);
    assert!(dir.child_dir("missing", false).unwrap().is_none());
}

#[test]
fn hard_links_are_counted_and_single_link_reads_and_creation_refuse_them() {
    let temp = private_test_home().unwrap();
    let dir = SafeDir::open_nofollow(temp.path(), false).unwrap();
    dir.create_file_new("first", b"secret").unwrap();
    std::fs::hard_link(temp.path().join("first"), temp.path().join("second")).unwrap();
    assert_eq!(dir.entry("first").unwrap().unwrap().nlink, 2);
    assert!(dir.list().unwrap().iter().all(|entry| entry.nlink == 2));
    assert!(dir.open_read_checked("first", true).is_err());
    assert!(dir.read_bytes_checked("first", 100, true).is_err());
    assert!(dir.create_file_new("first", b"changed").is_err());
    assert_eq!(dir.read_file("second").unwrap().as_deref(), Some("secret"));
    dir.remove_file("first").unwrap();
    assert_eq!(dir.entry("second").unwrap().unwrap().nlink, 1);
}

#[test]
fn child_keeps_ancestors_pinned_after_parent_is_dropped() {
    let temp = private_test_home().unwrap();
    let path = temp.path().join("parent");
    let renamed = temp.path().join("renamed");
    let dir = SafeDir::open_nofollow(&path, true).unwrap();
    let child = dir.child_dir("child", true).unwrap().unwrap();
    drop(dir);
    assert!(std::fs::rename(&path, &renamed).is_err());
    assert!(std::fs::rename(path.join("child"), path.join("other")).is_err());
    child.create_file_new("file", b"safe").unwrap();
    drop(child);
    std::fs::rename(&path, &renamed).unwrap();
    assert_eq!(std::fs::read(renamed.join("child/file")).unwrap(), b"safe");
}

#[test]
fn junctions_are_never_traversed_read_written_or_removed() {
    let temp = private_test_home().unwrap();
    let outside = private_test_home().unwrap();
    std::fs::write(outside.path().join("victim"), b"secret").unwrap();
    let junction = temp.path().join("junction");
    let output = std::process::Command::new("cmd").args(["/d", "/c", "mklink", "/J"])
        .arg(&junction).arg(outside.path()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let dir = SafeDir::open_nofollow(temp.path(), false).unwrap();
    for create in [false, true] {
        assert!(SafeDir::open_nofollow(&junction.join("nested"), create).is_err());
        assert!(dir.child_dir("junction", create).is_err());
    }
    assert_eq!(dir.entry("junction").unwrap().unwrap().kind, EntryKind::Other);
    assert_eq!(dir.list().unwrap()[0].kind, EntryKind::Other);
    assert!(dir.open_read_checked("junction", false).is_err());
    assert!(dir.open_create_new("junction").is_err());
    assert!(dir.create_file_new("junction", b"bad").is_err());
    assert!(dir.write_file_atomic("junction", "bad").is_err());
    assert!(dir.remove_file("junction").is_err());
    assert!(!outside.path().join("nested").exists());
    assert_eq!(std::fs::read(outside.path().join("victim")).unwrap(), b"secret");
    std::fs::remove_dir(&junction).unwrap();
}

#[test]
fn rejects_nonfiles_stream_names_traversal_and_network_roots() {
    let temp = private_test_home().unwrap();
    let dir = SafeDir::open_nofollow(temp.path(), false).unwrap();
    dir.child_dir("directory", true).unwrap();
    assert!(dir.open_read_checked("directory", false).is_err());
    assert!(dir.create_file_new("directory", b"bad").is_err());
    assert!(dir.remove_file("directory").is_err());
    for name in ["..", "../escape", "C:escape", "file:stream", ".. ", "file."] {
        assert!(dir.open_create_new(name).is_err(), "{name}");
        assert!(dir.child_dir(name, true).is_err(), "{name}");
        assert!(dir.open_read_checked(name, false).is_err(), "{name}");
    }
    for path in [r"\\server\share\dir", r"\\?\UNC\server\share\dir", r"\\.\C:\dir"] {
        assert!(SafeDir::open_nofollow(Path::new(path), false).is_err());
    }
    dir.create_file_new("file", b"data").unwrap();
    assert!(SafeDir::open_nofollow(&temp.path().join("file/nested"), true).is_err());
    assert!(SafeDir::open_nofollow(Path::new(&format!(r"{}\..\escape", temp.path().display())), true).is_err());
}

// Convert this exact directory object in place, without deleting or renaming it.
fn set_junction(path: &Path, target: &Path, access: u32) -> io::Result<()> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_SHARE_DELETE};
    use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT};
    let file = std::fs::OpenOptions::new().access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(path)?;
    let target = target.to_str().unwrap();
    let target = target.strip_prefix(r"\\?\").unwrap_or(target);
    let substitute: Vec<u16> = format!(r"\??\{target}").encode_utf16().collect();
    let print: Vec<u16> = target.encode_utf16().collect();
    // REPARSE_DATA_BUFFER mount point: tag, data length, reserved, four u16
    // offsets/lengths, then NUL-terminated substitute and print names.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&0xA0000003u32.to_le_bytes());
    buffer.extend_from_slice(&((8 + (substitute.len() + print.len() + 2) * 2) as u16).to_le_bytes());
    for value in [0, 0, substitute.len() as u16 * 2, (substitute.len() as u16 + 1) * 2, print.len() as u16 * 2] {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    for value in substitute.iter().chain(std::iter::once(&0)).chain(print.iter()).chain(std::iter::once(&0)) {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    let mut returned = 0;
    // SAFETY: live writable directory handle, complete mount-point buffer and
    // writable byte count; no overlapped operation or output data requested.
    if unsafe { DeviceIoControl(file.as_raw_handle(), FSCTL_SET_REPARSE_POINT,
        buffer.as_ptr().cast(), buffer.len() as u32, std::ptr::null_mut(), 0,
        &mut returned, std::ptr::null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[test]
fn in_place_junction_conversion_cannot_redirect_a_pinned_empty_directory() {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA}};
    struct JunctionGuard(std::path::PathBuf);
    impl Drop for JunctionGuard {
        fn drop(&mut self) {
            // Nonrecursive removal deletes only the junction, never its target.
            if let Err(error) = std::fs::remove_dir(&self.0) {
                if !std::thread::panicking() { panic!("junction cleanup failed: {error}"); }
                eprintln!("junction cleanup failed during panic: {error}");
            }
        }
    }
    for access in [GENERIC_WRITE, FILE_WRITE_ATTRIBUTES | FILE_WRITE_DATA] {
        let temp = private_test_home().unwrap();
        let outside = private_test_home().unwrap();
        std::fs::write(outside.path().join("victim"), b"secret").unwrap();
        std::fs::create_dir(outside.path().join("subdir")).unwrap();
        // Prove the reparse buffer works on this filesystem before testing the
        // pinned object. Both writable access masks must exercise the attack.
        let control = temp.path().join("control");
        std::fs::create_dir(&control).unwrap();
        let control_cleanup = JunctionGuard(control.clone());
        set_junction(&control, outside.path(), GENERIC_WRITE).unwrap();
        assert_eq!(std::fs::read(control.join("victim")).unwrap(), b"secret");
        drop(control_cleanup);
        let path = temp.path().join("pinned");
        std::fs::create_dir(&path).unwrap();
        // Drop SafeDir first so its no-delete-sharing handles cannot block cleanup.
        let _cleanup = JunctionGuard(path.clone());
        let dir = SafeDir::open_nofollow(&path, false).unwrap();
        set_junction(&path, outside.path(), access).unwrap();
        assert_ne!(std::fs::symlink_metadata(&path).unwrap().file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        assert_eq!(std::fs::read(path.join("victim")).unwrap(), b"secret");
        assert!(!matches!(dir.entry("victim"), Ok(Some(_))));
        assert!(!matches!(dir.open_read_checked("victim", true), Ok(Some(_))));
        assert!(!matches!(dir.read_bytes_checked("victim", 100, true), Ok(Some(_))));
        assert!(!matches!(dir.child_dir("subdir", false), Ok(Some(_))));
        let _ = dir.child_dir("created-dir", true);
        let _ = dir.open_create_new("created-open");
        let _ = dir.create_file_new("created", b"safe");
        let _ = dir.write_file_atomic("victim", "replacement");
        if let Ok(entries) = dir.list() {
            assert!(!entries.iter().any(|entry| entry.name == "subdir"));
        }
        let _ = dir.remove_file("victim");
        assert_eq!(std::fs::read(outside.path().join("victim")).unwrap(), b"secret");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 2);
        drop(dir);
    }
}

#[test]
fn rejects_reserved_devices_and_illegal_windows_names() {
    let temp = private_test_home().unwrap();
    let dir = SafeDir::open_nofollow(temp.path(), false).unwrap();
    let mut names: Vec<String> = ["CON", "prn.txt", "AuX", "nul.tar.gz", "con .txt", "COM¹", "com².txt", "LPT³.log",
        "a<b", "a>b", "a\"b", "a|b", "a?b", "a*b", "a\u{1f}b", "a\u{7f}b"].into_iter().map(str::to_string).collect();
    for prefix in ["COM", "LPT"] {
        for number in 1..=9 {
            names.push(format!("{prefix}{number}"));
            names.push(format!("{}{number}.txt", prefix.to_ascii_lowercase()));
        }
        for number in ["¹", "²", "³"] { names.push(format!("{prefix}{number}.txt")); }
    }
    for name in names {
        assert_eq!(check_name(&name).unwrap_err().kind(), io::ErrorKind::PermissionDenied, "{name}");
        assert!(dir.entry(&name).is_err(), "{name}");
        assert!(dir.open_read_checked(&name, false).is_err(), "{name}");
        assert!(dir.open_create_new(&name).is_err(), "{name}");
        assert!(dir.child_dir(&name, true).is_err(), "{name}");
        assert!(dir.write_file_atomic(&name, "bad").is_err(), "{name}");
        assert!(dir.remove_file(&name).is_err(), "{name}");
    }
    for name in ["console", "COM0", "COM10", "LPT0.txt", "LPT10", "auxiliary.txt"] {
        assert!(dir.create_file_new(name, b"allowed").unwrap(), "{name}");
    }
    assert_eq!(dir.list().unwrap().len(), 6);
}

#[test]
fn atomic_write_faults_leave_the_old_file_and_no_temporary_entries() {
    let temp = private_test_home().unwrap();
    let dir = SafeDir::open_nofollow(temp.path(), false).unwrap();
    dir.write_file_atomic("file", "old").unwrap();
    for step in [WriteStep::Write, WriteStep::Sync, WriteStep::Rename] {
        let fault = |current| if current == step { Err(io::Error::other("injected")) } else { Ok(()) };
        assert!(dir.write_bytes_atomic_with("file", b"new", &tmp_name("file"), &fault).is_err());
        assert_eq!(dir.read_file("file").unwrap().as_deref(), Some("old"));
        assert_eq!(dir.list().unwrap().len(), 1, "{step:?}");
    }
    dir.child_dir("directory", true).unwrap();
    assert!(dir.write_file_atomic("directory", "bad").is_err());
    assert_eq!(dir.list().unwrap().len(), 2);
}

#[test]
fn listing_restarts_and_continues_from_the_pinned_handle() {
    let temp = private_test_home().unwrap();
    let dir = SafeDir::open_nofollow(temp.path(), false).unwrap();
    // More than one 64 KiB enumeration buffer, including UTF-16 names.
    for i in 0..700 { dir.create_file_new(&format!("entry-{i:04}-交付.txt"), b"x").unwrap(); }
    let expected = dir.list().unwrap();
    assert_eq!(expected.len(), 700);
    assert_eq!(dir.list().unwrap(), expected);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| dir.list().unwrap());
        let second = scope.spawn(|| dir.list().unwrap());
        assert_eq!(first.join().unwrap(), expected);
        assert_eq!(second.join().unwrap(), expected);
    });
}

#[test]
fn readonly_target_failure_cleans_up_the_temporary_file() {
    let temp = private_test_home().unwrap();
    let dir = SafeDir::open_nofollow(temp.path(), false).unwrap();
    dir.create_file_new("readonly", b"old").unwrap();
    let file = dir.open_read_checked("readonly", false).unwrap().unwrap();
    let mut perms = file.metadata().unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(temp.path().join("readonly"), perms).unwrap();
    drop(file);
    for step in [WriteStep::Sync, WriteStep::Rename] {
        let fault = |current| if current == step { Err(io::Error::other("injected")) } else { Ok(()) };
        assert!(dir.write_bytes_atomic_with("readonly", b"new", &tmp_name("readonly"), &fault).is_err());
        assert_eq!(dir.list().unwrap().len(), 1, "{step:?}");
    }
    assert!(dir.write_file_atomic("readonly", "new").is_err());
    assert_eq!(dir.list().unwrap().len(), 1);
    assert_eq!(dir.read_file("readonly").unwrap().as_deref(), Some("old"));
    let mut perms = std::fs::metadata(temp.path().join("readonly")).unwrap().permissions();
    perms.set_readonly(false);
    std::fs::set_permissions(temp.path().join("readonly"), perms).unwrap();
}
