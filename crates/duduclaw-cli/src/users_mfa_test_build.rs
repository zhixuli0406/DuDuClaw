//! TEST-key builds only (owner decision E10, 2026-11-20): the first
//! Administrator's one-time setup code for unattended live acceptance of an
//! installed TEST build, `duduclaw ops users mfa-setup-code-test --out <path>`.
//!
//! This whole module, its clap variant ([`super::UsersCommands`]), its Bash-lane
//! entry and every string in it exist only in a `--cfg duduclaw_test_root`
//! build (the build that also accepts TEST licences and carries the TEST
//! licence marker the release pipeline refuses). A production build compiles
//! none of it; `scripts/refuse-test-license-build.py` additionally refuses any
//! gateway binary containing [`SUBCOMMAND`] or [`NOTICE`].
//!
//! Gates, in this order, before anything is opened: Linux; effective uid 0;
//! the running binary reports a TEST-licence build with a compiled TEST issuer
//! (`license_runtime::license_build_identity`, the identity the licence code
//! derives from the same compiled TEST issuer it trusts); `DUDUCLAW_ISO_DISPOSABLE=1`.
//! Unlike `mfa-setup-code` it does not refuse AI-session markers (the
//! acceptance driver may be an AI-operated shell) and does not need a
//! terminal. The code is issued by the same `mfa_issue_bootstrap_token` under
//! its own audit actor and goes only to `--out`: a new file (O_CREAT|O_EXCL|
//! O_NOFOLLOW, 0600, owned by root) in a root-owned directory not writable by
//! group or other, created before the code is issued and removed again when
//! nothing was issued. stdout stays empty; stderr gets [`NOTICE`] and one
//! fixed result line. Exit: 0 issued, 3 not needed, 1 refused or failed.

use std::path::Path;

use zeroize::Zeroizing;

/// The sub-command name (also a forbidden release marker).
pub(super) const SUBCOMMAND: &str = "mfa-setup-code-test";
/// Printed on stderr on every invocation (also a forbidden release marker).
pub(super) const NOTICE: &str = "TEST BUILD ONLY: issuing a first-run setup code for automated acceptance";
const ISSUED_LINE: &str = "TEST BUILD ONLY: setup code written to --out";
const NOT_NEEDED_LINE: &str =
    "TEST BUILD ONLY: no setup code needed (two-factor sign-in not enforced, or no single first Administrator)";
const REFUSED_PREFIX: &str = "TEST BUILD ONLY: refused: ";
/// Audit actor of `mfa.bootstrap_code_issued`, distinct from the host command's.
pub(super) const ACTOR: &str = "測試建置的自動驗收（身分未驗證）";
/// The disposable-host acknowledgement of the root tests and acceptance harnesses.
pub(super) const DISPOSABLE_ENV: &str = "DUDUCLAW_ISO_DISPOSABLE";

pub(super) const OPERATOR_COMMAND: duduclaw_core::OperatorCommand = duduclaw_core::OperatorCommand {
    name: "duduclaw ops users mfa-setup-code-test",
    path: &["ops", "users", SUBCOMMAND],
};

pub(super) const EXIT_ISSUED: i32 = 0;
pub(super) const EXIT_NOT_NEEDED: i32 = 3;
pub(super) const EXIT_REFUSED: i32 = 1;

/// Closed set of refusal reasons; the label is the only variable part of the
/// failure line (never a path, error text or secret).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    NotLinux,
    NotRoot,
    NotTestBuild,
    NotDisposable,
    OutInvalid,
    OutParentUnsafe,
    OutExists,
    OutCreateFailed,
    UsersDbMissing,
    UsersDbUnavailable,
    BootstrapClosed,
    IssueFailed,
    WriteFailed,
}

impl Refusal {
    fn label(self) -> &'static str {
        match self {
            Self::NotLinux => "not_linux",
            Self::NotRoot => "not_root",
            Self::NotTestBuild => "not_test_licence_build",
            Self::NotDisposable => "disposable_host_not_acknowledged",
            Self::OutInvalid => "out_path_invalid",
            Self::OutParentUnsafe => "out_parent_not_root_owned_or_writable_by_others",
            Self::OutExists => "out_path_exists",
            Self::OutCreateFailed => "out_create_failed",
            Self::UsersDbMissing => "users_db_missing",
            Self::UsersDbUnavailable => "users_db_unavailable",
            Self::BootstrapClosed => "first_admin_setup_already_completed",
            Self::IssueFailed => "issue_failed",
            Self::WriteFailed => "write_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Issued,
    NotNeeded,
    Refused(Refusal),
}

impl Outcome {
    pub(super) fn exit_code(self) -> i32 {
        match self {
            Self::Issued => EXIT_ISSUED,
            Self::NotNeeded => EXIT_NOT_NEEDED,
            Self::Refused(_) => EXIT_REFUSED,
        }
    }

    fn line(self) -> String {
        match self {
            Self::Issued => ISSUED_LINE.to_owned(),
            Self::NotNeeded => NOT_NEEDED_LINE.to_owned(),
            Self::Refused(reason) => format!("{REFUSED_PREFIX}{}", reason.label()),
        }
    }
}

/// The identity and environment facts the gates read (injectable for tests).
#[derive(Debug, Clone, Copy)]
pub(super) struct Host {
    pub linux: bool,
    pub root: bool,
    pub test_build: bool,
    pub disposable: bool,
    /// Required owner of `--out` and its directory (0 outside tests).
    pub owner_uid: u32,
}

impl Host {
    fn current() -> Self {
        Self {
            linux: cfg!(target_os = "linux"),
            root: effective_uid() == Some(0),
            test_build: is_test_licence_build(),
            disposable: std::env::var_os(DISPOSABLE_ENV).is_some_and(|value| value.as_os_str() == std::ffi::OsStr::new("1")),
            owner_uid: 0,
        }
    }
}

#[cfg(unix)]
fn effective_uid() -> Option<u32> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn effective_uid() -> Option<u32> {
    None
}

/// True when this binary trusts a compiled TEST licence issuer: the licence
/// build identity is `TEST-LICENSE <64 hex>` (`license_build_identity` prints
/// exactly the key `production_registry` registers; `TEST-LICENSE none`, a
/// test-root build without a TEST issuer, accepts no TEST licence and is refused).
pub(super) fn is_test_licence_build() -> bool {
    test_licence_identity(&duduclaw_gateway::license_runtime::license_build_identity())
}

fn test_licence_identity(identity: &str) -> bool {
    identity
        .strip_prefix("TEST-LICENSE ")
        .is_some_and(|key| key.len() == 64 && key.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// Entry point of the sub-command: returns the process exit code.
pub(super) fn run(home: &Path, out: &Path) -> i32 {
    eprintln!("{NOTICE}");
    let outcome = issue(home, out, &Host::current(), chrono::Utc::now().timestamp());
    eprintln!("{}", outcome.line());
    outcome.exit_code()
}

fn gate(host: &Host) -> Result<(), Refusal> {
    if !host.linux {
        return Err(Refusal::NotLinux);
    }
    if !host.root {
        return Err(Refusal::NotRoot);
    }
    if !host.test_build {
        return Err(Refusal::NotTestBuild);
    }
    if !host.disposable {
        return Err(Refusal::NotDisposable);
    }
    Ok(())
}

pub(super) fn issue(home: &Path, out: &Path, host: &Host, now: i64) -> Outcome {
    if let Err(reason) = gate(host) {
        return Outcome::Refused(reason);
    }
    // The destination exists before a code does, so a bad --out never mints
    // (or replaces) a code that could not be delivered.
    let file = match OutFile::create(out, host.owner_uid) {
        Ok(file) => file,
        Err(reason) => return Outcome::Refused(reason),
    };
    let outcome = issue_into(home, &file, now);
    if outcome != Outcome::Issued {
        file.discard();
    }
    outcome
}

fn issue_into(home: &Path, file: &OutFile, now: i64) -> Outcome {
    let db_path = home.join("users.db");
    if !db_path.is_file() {
        return Outcome::Refused(Refusal::UsersDbMissing);
    }
    let Ok(db) = duduclaw_auth::UserDb::new(&db_path) else {
        return Outcome::Refused(Refusal::UsersDbUnavailable);
    };
    let code = match db.mfa_issue_bootstrap_token(ACTOR, now) {
        Ok(Some(code)) => Zeroizing::new(code),
        Ok(None) => return Outcome::NotNeeded,
        Err(error) if error == duduclaw_auth::mfa::BOOTSTRAP_CLOSED => return Outcome::Refused(Refusal::BootstrapClosed),
        Err(_) => return Outcome::Refused(Refusal::IssueFailed),
    };
    match file.write_code(&code) {
        Ok(()) => Outcome::Issued,
        Err(reason) => Outcome::Refused(reason),
    }
}

/// The freshly created `--out` file, held through its directory descriptor.
#[cfg(unix)]
struct OutFile {
    dir: std::fs::File,
    name: std::ffi::CString,
    file: std::fs::File,
}

#[cfg(unix)]
impl OutFile {
    fn create(out: &Path, owner: u32) -> Result<Self, Refusal> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        if !out.is_absolute()
            || out.as_os_str().as_bytes().ends_with(b"/")
            || !out.components().all(|c| matches!(c, std::path::Component::RootDir | std::path::Component::Normal(_)))
        {
            return Err(Refusal::OutInvalid);
        }
        let (Some(parent), Some(name)) = (out.parent(), out.file_name()) else {
            return Err(Refusal::OutInvalid);
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| Refusal::OutInvalid)?;
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|_| Refusal::OutParentUnsafe)?;
        let meta = dir.metadata().map_err(|_| Refusal::OutParentUnsafe)?;
        if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
            return Err(Refusal::OutParentUnsafe);
        }
        // SAFETY: `dir` is an open directory descriptor and `name` a valid
        // NUL-terminated single path component; the mode is passed as c_uint
        // (variadic promotion).
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::EEXIST) {
                Refusal::OutExists
            } else {
                Refusal::OutCreateFailed
            });
        }
        // SAFETY: `fd` was just returned by openat and is owned by nothing else.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let created = Self { dir, name, file };
        // Exactly 0600 whatever the umask; a regular, single-link file owned by `owner`.
        let checked = created.file.set_permissions(std::fs::Permissions::from_mode(0o600)).is_ok()
            && created.file.metadata().is_ok_and(|meta| {
                meta.is_file() && meta.uid() == owner && meta.mode() & 0o7777 == 0o600 && meta.nlink() == 1
            });
        if !checked {
            created.discard();
            return Err(Refusal::OutCreateFailed);
        }
        Ok(created)
    }

    fn write_code(&self, code: &str) -> Result<(), Refusal> {
        use std::io::Write;
        let mut file = &self.file;
        file.write_all(code.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| self.file.sync_all())
            .map_err(|_| Refusal::WriteFailed)?;
        let _ = self.dir.sync_all();
        Ok(())
    }

    /// Remove the file this run created (nothing, or nothing usable, in it).
    fn discard(self) {
        use std::os::fd::AsRawFd;
        // SAFETY: valid directory descriptor and NUL-terminated name.
        unsafe {
            libc::unlinkat(self.dir.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}

/// Not Unix: never reached past the Linux gate; refuses anyway.
#[cfg(not(unix))]
struct OutFile;

#[cfg(not(unix))]
impl OutFile {
    fn create(_out: &Path, _owner: u32) -> Result<Self, Refusal> {
        Err(Refusal::NotLinux)
    }

    fn write_code(&self, _code: &str) -> Result<(), Refusal> {
        Err(Refusal::NotLinux)
    }

    fn discard(self) {}
}

/// Run only in a test-root build:
/// `RUSTFLAGS='--cfg duduclaw_test_root' CARGO_TARGET_DIR=target/test-root
///  cargo test -p duduclaw-cli --lib users_mfa_cmd`
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn me() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    fn host() -> Host {
        Host { linux: true, root: true, test_build: true, disposable: true, owner_uid: me() }
    }

    /// An enforced, claimed installation nobody has enrolled on yet.
    fn pending_bootstrap() -> (tempfile::TempDir, duduclaw_auth::UserDb) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[auth]\nmfa_enforced=true\n").unwrap();
        let db = duduclaw_auth::UserDb::new_with_mfa_default(&dir.path().join("users.db"), true).unwrap();
        db.ensure_default_admin().unwrap();
        assert!(db.claim_default_admin_with_email(Some("owner@example.test"), "password-test").unwrap());
        (dir, db)
    }

    /// A private 0700 output directory owned by the test user.
    fn out_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn issued_rows(db: &duduclaw_auth::UserDb) -> Vec<duduclaw_auth::AuditEntry> {
        db.query_audit_log(None, Some("mfa.bootstrap_code_issued"), 10).unwrap()
    }

    #[test]
    fn mfa_setup_code_test_gates_refuse_in_order_before_anything_is_opened() {
        let home = tempfile::tempdir().unwrap();
        let out_dir = out_dir();
        let out = out_dir.path().join("code");
        let cases = [
            (Host { linux: false, ..host() }, Refusal::NotLinux),
            (Host { linux: false, root: false, test_build: false, disposable: false, ..host() }, Refusal::NotLinux),
            (Host { root: false, ..host() }, Refusal::NotRoot),
            (Host { root: false, test_build: false, disposable: false, ..host() }, Refusal::NotRoot),
            (Host { test_build: false, ..host() }, Refusal::NotTestBuild),
            (Host { test_build: false, disposable: false, ..host() }, Refusal::NotTestBuild),
            (Host { disposable: false, ..host() }, Refusal::NotDisposable),
        ];
        for (host, expected) in cases {
            let outcome = issue(home.path(), &out, &host, now());
            assert_eq!(outcome, Outcome::Refused(expected), "{host:?}");
            assert_eq!(outcome.exit_code(), EXIT_REFUSED);
            assert!(!out.exists(), "nothing created for {host:?}");
            assert!(!home.path().join("users.db").exists(), "users.db not opened for {host:?}");
        }
    }

    #[test]
    fn mfa_setup_code_test_issues_once_into_a_new_0600_file_with_its_own_audit_actor() {
        let (home, db) = pending_bootstrap();
        let out_dir = out_dir();
        let out = out_dir.path().join("setup-code");
        let outcome = issue(home.path(), &out, &host(), now());
        assert_eq!(outcome, Outcome::Issued);
        assert_eq!(outcome.exit_code(), EXIT_ISSUED);
        let meta = std::fs::symlink_metadata(&out).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.mode() & 0o7777, 0o600);
        assert_eq!(meta.uid(), me());
        let text = std::fs::read_to_string(&out).unwrap();
        let code = text.strip_suffix('\n').unwrap();
        assert_eq!(code.len(), 32);
        assert!(code.bytes().all(|b| b.is_ascii_hexdigit()));
        let rows = issued_rows(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].detail.as_deref(), Some(ACTOR));
        assert!(rows.iter().all(|row| !format!("{row:?}").contains(code)), "the code never reaches the audit row");
        // The output path is never reused or overwritten.
        assert_eq!(issue(home.path(), &out, &host(), now()), Outcome::Refused(Refusal::OutExists));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
        assert_eq!(issued_rows(&db).len(), 1, "no code minted for a refused --out");
    }

    #[test]
    fn mfa_setup_code_test_not_needed_and_closed_latch_leave_no_file() {
        // Not enforced: nothing to issue, exit 3, the empty file removed again.
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        db.ensure_default_admin().unwrap();
        let out_dir = out_dir();
        let out = out_dir.path().join("code");
        let outcome = issue(home.path(), &out, &host(), now());
        assert_eq!(outcome, Outcome::NotNeeded);
        assert_eq!(outcome.exit_code(), EXIT_NOT_NEEDED);
        assert!(!out.exists());
        assert!(issued_rows(&db).is_empty());

        // Closed latch: refused, no file.
        let (home, db) = pending_bootstrap();
        let owner = db.get_bootstrap_owner().unwrap().unwrap();
        rusqlite::Connection::open(home.path().join("users.db")).unwrap()
            .execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'enc',0)", [&owner.id])
            .unwrap();
        assert_eq!(issue(home.path(), &out, &host(), now()), Outcome::Refused(Refusal::BootstrapClosed));
        assert!(!out.exists());

        // No users.db in the home: refused, no file, nothing created there.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(issue(empty.path(), &out, &host(), now()), Outcome::Refused(Refusal::UsersDbMissing));
        assert!(!out.exists());
        assert!(!empty.path().join("users.db").exists());
    }

    #[test]
    fn mfa_setup_code_test_refuses_unsafe_destinations_without_issuing() {
        let (home, db) = pending_bootstrap();
        let out_dir = out_dir();
        // Relative, dot-dot, trailing slash.
        for bad in ["relative/code", "/tmp/../code", "/tmp/code/"] {
            assert_eq!(issue(home.path(), Path::new(bad), &host(), now()), Outcome::Refused(Refusal::OutInvalid), "{bad}");
        }
        // A symlink at the path is never followed.
        let target = out_dir.path().join("target");
        let link = out_dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(issue(home.path(), &link, &host(), now()), Outcome::Refused(Refusal::OutExists));
        assert!(!target.exists());
        // A pre-existing file is never overwritten.
        let existing = out_dir.path().join("existing");
        std::fs::write(&existing, "keep").unwrap();
        assert_eq!(issue(home.path(), &existing, &host(), now()), Outcome::Refused(Refusal::OutExists));
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "keep");
        // Parent writable by group or other, owned by someone else, or missing.
        for mode in [0o770, 0o707, 0o777] {
            std::fs::set_permissions(out_dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(issue(home.path(), &out_dir.path().join("code"), &host(), now()),
                Outcome::Refused(Refusal::OutParentUnsafe), "{mode:o}");
        }
        std::fs::set_permissions(out_dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let other_owner = Host { owner_uid: me().wrapping_add(1), ..host() };
        assert_eq!(issue(home.path(), &out_dir.path().join("code"), &other_owner, now()),
            Outcome::Refused(Refusal::OutParentUnsafe));
        assert_eq!(issue(home.path(), &out_dir.path().join("missing/code"), &host(), now()),
            Outcome::Refused(Refusal::OutParentUnsafe));
        // A symlinked parent directory (last component) is not followed either.
        let real = out_dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = out_dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert_eq!(issue(home.path(), &alias.join("code"), &host(), now()), Outcome::Refused(Refusal::OutParentUnsafe));
        assert!(!real.join("code").exists());
        assert!(issued_rows(&db).is_empty(), "no code minted for any refused destination");
    }

    #[test]
    fn mfa_setup_code_test_reuses_the_licence_build_identity() {
        let key = "0123456789abcdef".repeat(4);
        assert!(test_licence_identity(&format!("TEST-LICENSE {key}")));
        for bad in [String::from("TEST-LICENSE none"), format!("PRODUCTION-LICENSE {}", &key[..16]),
            format!("TEST-LICENSE {}", key.to_uppercase()), format!("TEST-LICENSE {key}\n"),
            format!("TEST-LICENSE  {key}"), format!("TEST-LICENSE {}", &key[..62]), String::new()] {
            assert!(!test_licence_identity(&bad), "{bad:?}");
        }
        let identity = duduclaw_gateway::license_runtime::license_build_identity();
        assert!(identity.starts_with("TEST-LICENSE "), "a test-root build reports TEST-LICENSE: {identity}");
        assert_eq!(is_test_licence_build(), identity != "TEST-LICENSE none");
    }

    #[test]
    fn mfa_setup_code_test_exists_in_the_test_root_build_and_is_a_release_marker() {
        use clap::{CommandFactory, Parser};
        let cli = crate::Cli::command();
        let users = cli.find_subcommand("ops").unwrap().find_subcommand("users").unwrap();
        assert!(users.find_subcommand(SUBCOMMAND).is_some());
        assert!(crate::Cli::try_parse_from(["duduclaw", "ops", "users", SUBCOMMAND, "--out", "/root/code"]).is_ok());
        assert!(crate::Cli::try_parse_from(["duduclaw", "ops", "users", SUBCOMMAND]).is_err(), "--out is required");
        assert!(super::super::OPERATOR_COMMANDS.iter().any(|entry| entry.path == OPERATOR_COMMAND.path));
        // Read at run time (not embedded): scripts/check_embedded_paths.py.
        let refuser = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/refuse-test-license-build.py"),
        )
        .unwrap();
        for marker in [SUBCOMMAND, NOTICE] {
            assert!(refuser.contains(&format!("b\"{marker}\"")), "{marker} must be a forbidden release marker");
        }
        for caller in [duduclaw_core::HookCaller::Agent("alice".into()), duduclaw_core::HookCaller::Untrusted("unknown".into())] {
            assert!(super::super::bash_decision("duduclaw ops users mfa-setup-code-test --out /root/code", &caller).is_some());
        }
        // Exit codes and the fixed lines.
        assert_eq!((Outcome::Issued.exit_code(), Outcome::NotNeeded.exit_code()), (0, 3));
        assert!(Outcome::Refused(Refusal::NotRoot).line().starts_with(REFUSED_PREFIX));
        assert!([Outcome::Issued.line(), Outcome::NotNeeded.line(), NOTICE.to_owned()].iter().all(|line| !line.contains('\n')));
    }
}
