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
