//! R8r-2 writer tests, unprivileged: a temporary directory stands in for the
//! attached image's verified home and the test user for the employee.
use super::*;
use std::{fs, os::unix::fs::{MetadataExt, PermissionsExt}, path::{Path, PathBuf}};

const DOCUMENT: &[u8] = br#"{"providers":[{"name":"main","api_key":"sk-test-0001","base_url":"https://llm.example/v1"}]}"#;

struct Home { _temp: tempfile::TempDir, path: PathBuf, fd: OwnedFd, owner: Owner }
fn home() -> Home {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("home");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let fd = OwnedFd::from(File::open(&path).unwrap());
    let owner = Owner { uid: unsafe { libc::getuid() }, gid: unsafe { libc::getgid() } };
    Home { _temp: temp, path, fd, owner }
}
fn temp(byte: u8) -> String { crate::storage::providers_temp_name([byte; 8]) }
impl Home {
    fn api(&self) -> PathBuf { self.path.join(DIRECTORY) }
    fn file(&self) -> PathBuf { self.api().join(FILE) }
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.api()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort(); names
    }
    fn write(&self, byte: u8, bytes: &[u8]) -> Result<(), ProvidersFailure> { write(&self.fd, self.owner, &temp(byte), bytes) }
}
fn mode(path: &Path) -> u32 { fs::symlink_metadata(path).unwrap().mode() & 0o7777 }
fn mkfifo(path: &Path) {
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
}

#[test]
fn providers_fresh_home_gets_a_private_directory_and_the_exact_file() {
    let home = home();
    home.write(1, DOCUMENT).unwrap();
    let directory = fs::symlink_metadata(home.api()).unwrap();
    assert!(directory.is_dir());
    assert_eq!((directory.uid(), directory.gid(), mode(&home.api())), (home.owner.uid, home.owner.gid, 0o700));
    let file = fs::symlink_metadata(home.file()).unwrap();
    assert!(file.is_file());
    assert_eq!((file.uid(), file.gid(), mode(&home.file()), file.nlink()), (home.owner.uid, home.owner.gid, 0o600, 1));
    assert_eq!(fs::read(home.file()).unwrap(), DOCUMENT);
    assert_eq!(home.names(), [FILE], "no temporary file is left");
}

#[test]
fn providers_every_call_rewrites_even_identical_bytes() {
    let home = home();
    home.write(1, DOCUMENT).unwrap();
    let first = fs::symlink_metadata(home.file()).unwrap().ino();
    home.write(2, DOCUMENT).unwrap();
    assert_ne!(fs::symlink_metadata(home.file()).unwrap().ino(), first, "a new file, never skipped");
    assert_eq!(fs::read(home.file()).unwrap(), DOCUMENT);
}

#[test]
fn providers_existing_target_is_replaced_without_being_opened() {
    // A FIFO: opening it for writing would block; the call must not.
    let home = home();
    fs::create_dir(home.api()).unwrap();
    fs::set_permissions(home.api(), fs::Permissions::from_mode(0o700)).unwrap();
    mkfifo(&home.file());
    home.write(1, DOCUMENT).unwrap();
    assert!(fs::symlink_metadata(home.file()).unwrap().is_file());
    assert_eq!(fs::read(home.file()).unwrap(), DOCUMENT);
    // A symlink to a file outside: the outside file is untouched, the link replaced.
    let outside = home.path.parent().unwrap().join("outside");
    fs::write(&outside, b"outside").unwrap();
    fs::remove_file(home.file()).unwrap();
    std::os::unix::fs::symlink(&outside, home.file()).unwrap();
    home.write(2, b"new").unwrap();
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    assert!(fs::symlink_metadata(home.file()).unwrap().is_file());
    assert_eq!(fs::read(home.file()).unwrap(), b"new");
    // A hard-linked target: replaced; the other link keeps the old content.
    let other = home.path.join("other-link");
    fs::hard_link(home.file(), &other).unwrap();
    home.write(3, DOCUMENT).unwrap();
    assert_eq!(fs::read(&other).unwrap(), b"new");
    assert_eq!(fs::read(home.file()).unwrap(), DOCUMENT);
    assert_eq!(fs::symlink_metadata(home.file()).unwrap().nlink(), 1);
    assert_eq!(home.names(), [FILE]);
}

#[test]
fn providers_unsafe_directory_entry_writes_nothing() {
    let outside_dir = |home: &Home| { let dir = home.path.parent().unwrap().join("elsewhere"); fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap(); dir };
    // A symlink to a private directory elsewhere.
    let home = home();
    let elsewhere = outside_dir(&home);
    std::os::unix::fs::symlink(&elsewhere, home.api()).unwrap();
    assert_eq!(home.write(1, DOCUMENT), Err(ProvidersFailure::HomeEntryUnsafe));
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing created through the link");
    assert!(fs::symlink_metadata(home.api()).unwrap().file_type().is_symlink());
    // A regular file.
    let home = self::home();
    fs::write(home.api(), b"x").unwrap();
    assert_eq!(home.write(1, DOCUMENT), Err(ProvidersFailure::HomeEntryUnsafe));
    assert_eq!(fs::read(home.api()).unwrap(), b"x");
    // A FIFO (never blocks).
    let home = self::home();
    mkfifo(&home.api());
    assert_eq!(home.write(1, DOCUMENT), Err(ProvidersFailure::HomeEntryUnsafe));
    // A directory with another mode.
    let home = self::home();
    fs::create_dir(home.api()).unwrap();
    fs::set_permissions(home.api(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(home.write(1, DOCUMENT), Err(ProvidersFailure::HomeEntryUnsafe));
    assert!(home.names().is_empty(), "nothing created");
    // Another owner (the expected uid is the employee's, here: not the test user).
    fs::set_permissions(home.api(), fs::Permissions::from_mode(0o700)).unwrap();
    let stranger = Owner { uid: home.owner.uid.wrapping_add(1), gid: home.owner.gid };
    assert_eq!(write(&home.fd, stranger, &temp(1), DOCUMENT), Err(ProvidersFailure::HomeEntryUnsafe));
    let stranger = Owner { uid: home.owner.uid, gid: home.owner.gid.wrapping_add(1) };
    assert_eq!(write(&home.fd, stranger, &temp(1), DOCUMENT), Err(ProvidersFailure::HomeEntryUnsafe));
    assert!(home.names().is_empty());
    // A directory at providers.json: the rename fails, the temp file is removed.
    fs::create_dir(home.file()).unwrap();
    assert_eq!(home.write(1, DOCUMENT), Err(ProvidersFailure::HomeEntryUnsafe));
    assert_eq!(home.names(), [FILE]);
    assert!(fs::symlink_metadata(home.file()).unwrap().is_dir());
}

#[test]
fn providers_temp_file_is_removed_on_a_write_failure() {
    let home = home();
    home.write(1, b"old").unwrap();
    let result = write_with(&home.fd, home.owner, &temp(2), DOCUMENT.len() as u64, |file| {
        io::Write::write_all(file, &DOCUMENT[..10])?;
        Err(io::Error::from_raw_os_error(libc::ENOSPC))
    });
    assert_eq!(result, Err(ProvidersFailure::NoSpace));
    assert_eq!(home.names(), [FILE], "the temporary file is gone");
    assert_eq!(fs::read(home.file()).unwrap(), b"old", "the old document is unchanged");
    // A short write is caught by the metadata readback.
    let result = write_with(&home.fd, home.owner, &temp(3), DOCUMENT.len() as u64, |file| io::Write::write_all(file, b"x"));
    assert_eq!(result, Err(ProvidersFailure::Unavailable));
    // A temp name that already exists is someone else's: refused, never removed.
    let planted = home.api().join(temp(4));
    fs::write(&planted, b"planted").unwrap();
    assert!(home.write(4, DOCUMENT).is_err());
    assert_eq!(fs::read(&planted).unwrap(), b"planted");
    // Only the fixed temp shape is ever used.
    assert_eq!(write(&home.fd, home.owner, "../escape", DOCUMENT), Err(ProvidersFailure::Unavailable));
}

#[test]
fn providers_clear_removes_the_file_tolerates_absence_and_keeps_other_entries() {
    let home = home();
    assert_eq!(clear(&home.fd, home.owner), Ok(()), "nothing there");
    home.write(1, DOCUMENT).unwrap();
    clear(&home.fd, home.owner).unwrap();
    assert!(fs::symlink_metadata(home.api()).is_err(), "an empty directory is removed");
    assert_eq!(clear(&home.fd, home.owner), Ok(()), "absent again");
    home.write(1, DOCUMENT).unwrap();
    fs::write(home.api().join("notes"), b"keep").unwrap();
    clear(&home.fd, home.owner).unwrap();
    assert_eq!(home.names(), ["notes"], "never recursive");
    // An unsafe directory is refused for clear as well.
    let home = self::home();
    std::os::unix::fs::symlink("/", home.api()).unwrap();
    assert_eq!(clear(&home.fd, home.owner), Err(ProvidersFailure::HomeEntryUnsafe));
    // A directory at providers.json stays.
    let home = self::home();
    fs::create_dir(home.api()).unwrap();
    fs::set_permissions(home.api(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(home.file()).unwrap();
    assert_eq!(clear(&home.fd, home.owner), Err(ProvidersFailure::HomeEntryUnsafe));
    assert!(fs::symlink_metadata(home.file()).unwrap().is_dir());
}

#[test]
fn providers_recovery_removes_only_the_recorded_temp_file() {
    let home = home();
    assert_eq!(remove_temp(&home.fd, home.owner, &temp(1)), Ok(()), "no directory");
    home.write(1, DOCUMENT).unwrap();
    fs::write(home.api().join(temp(2)), b"left over").unwrap();
    fs::write(home.api().join(temp(3)), b"not recorded").unwrap();
    remove_temp(&home.fd, home.owner, &temp(2)).unwrap();
    let mut expected = vec![FILE.to_string(), temp(3)]; expected.sort();
    assert_eq!(home.names(), expected);
    assert_eq!(remove_temp(&home.fd, home.owner, &temp(2)), Ok(()), "already gone");
    assert_eq!(remove_temp(&home.fd, home.owner, "providers.json"), Err(ProvidersFailure::Unavailable));
    assert_eq!(fs::read(home.file()).unwrap(), DOCUMENT);
}
