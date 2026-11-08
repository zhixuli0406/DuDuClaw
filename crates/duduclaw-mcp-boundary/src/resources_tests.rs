use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::*, ids::{RunId, Sha256Digest}};
fn directory(path: &Path) -> File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).custom_flags(0x02000000).open(path).unwrap()
    }
    #[cfg(not(windows))]
    { File::open(path).unwrap() }
}
fn context() -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("u".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("u".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![ResourceGrant::Workspace {
            handle: Text::new("work".into()).unwrap() }]).unwrap(), policy_revision: 1,
        gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [0; 32], jti: [0; 16],
    })
}
#[test]
fn resource_paths_reject_absolute_parent_and_ambiguous_components() {
    for path in ["", "/etc/passwd", "../secret", "a/../b", "a/./b", "a//b", "a/", "a\\b", "a\0b"] {
        assert!(relative_path(path).is_err(), "{path:?}");
    }
    assert!(relative_path("notes/員工.md").is_ok());
}
#[test]
fn connector_grants_cannot_cross_personal_or_shared_bindings() {
    let mut record = context().record().clone();
    record.employee_binding = EmployeeBinding::Shared(Text::new("tenant-a".into()).unwrap());
    record.resources = List::new(vec![ResourceGrant::Connector {
        account: Text::new("account".into()).unwrap(),
        binding: EmployeeBinding::Shared(Text::new("tenant-b".into()).unwrap()),
    }]).unwrap();
    assert!(validate_grants(&record).is_err());
    record.resources = List::new(vec![ResourceGrant::Connector {
        account: Text::new("account".into()).unwrap(), binding: record.employee_binding.clone(),
    }]).unwrap();
    assert!(validate_grants(&record).is_ok());
    record.employee_binding = EmployeeBinding::Personal(Text::new("u".into()).unwrap());
    // v2 permits signing a shared-home grant for a Personal employee. Actual
    // use still requires the live product policy and BindingStore account check.
    assert!(validate_grants(&record).is_ok());
}
#[test]
fn ungranted_workspaces_and_other_employees_are_denied() {
    let dir = tempfile::tempdir().unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), EmployeeId([9; 16]), directory(dir.path())).unwrap();
    assert!(roots.open_read(&context(), "work", "file").is_err());
    assert!(roots.open_read(&context(), "unknown", "file").is_err());
    assert!(roots.open_directory(&context(), "work", None).is_err());
    assert!(roots.open_directory(&context(), "unknown", None).is_err());
}
#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_platform_refuses_without_path_fallback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), b"data").unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), context().record().employee, directory(dir.path())).unwrap();
    assert!(roots.open_read(&context(), "work", "file").is_err());
    assert!(roots.open_directory(&context(), "work", None).is_err());
}
#[cfg(target_os = "linux")]
#[test]
fn openat2_rejects_symlinks_hardlinks_directories_and_fifo() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::write(dir.path().join("file"), b"safe").unwrap();
    symlink("file", dir.path().join("link")).unwrap();
    std::fs::create_dir(dir.path().join("directory")).unwrap();
    rustix::fs::mknodat(rustix::fs::CWD, dir.path().join("fifo"), rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR, 0).unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), context().record().employee, File::open(dir.path()).unwrap()).unwrap();
    assert!(roots.open_read(&context(), "work", "file").is_ok());
    assert!(roots.open_directory(&context(), "work", None).is_ok());
    assert!(roots.open_directory(&context(), "work", Some("directory")).is_ok());
    assert!(roots.open_directory(&context(), "work", Some("file")).is_err());
    for path in ["link", "directory", "fifo"] { assert!(roots.open_read(&context(), "work", path).is_err()); }
    std::fs::hard_link(dir.path().join("file"), dir.path().join("hard")).unwrap();
    assert!(roots.open_read(&context(), "work", "hard").is_err());
}
#[cfg(target_os = "linux")]
#[test]
fn symlink_swap_during_openat2_never_reads_outside_root() {
    use std::{io::Read, os::unix::fs::symlink, sync::{Arc, atomic::{AtomicBool, Ordering}}};
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let outside = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::write(outside.path().join("secret"), b"secret").unwrap();
    std::fs::create_dir(root.path().join("inside")).unwrap();
    std::fs::write(root.path().join("inside/secret"), b"safe").unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), context().record().employee, File::open(root.path()).unwrap()).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let flag = running.clone();
    let path = root.path().to_owned();
    let target = outside.path().to_owned();
    let writer = std::thread::spawn(move || {
        while flag.load(Ordering::Relaxed) {
            std::fs::rename(path.join("inside"), path.join("saved")).unwrap();
            symlink(&target, path.join("inside")).unwrap();
            std::fs::remove_file(path.join("inside")).unwrap();
            std::fs::rename(path.join("saved"), path.join("inside")).unwrap();
        }
    });
    for _ in 0..2000 {
        if let Ok(mut file) = roots.open_read(&context(), "work", "inside/secret") {
            let mut text = String::new();
            file.read_to_string(&mut text).unwrap();
            assert_eq!(text, "safe");
        }
    }
    running.store(false, Ordering::Relaxed);
    writer.join().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires root and mount privileges; creates only temporary test mounts"]
fn iso_root_openat2_mount_crossing_devices_and_nodev() {
    use std::{ffi::CString, os::unix::{ffi::OsStrExt, net::UnixListener}};
    struct Mount(CString);
    impl Mount {
        fn new(path: &Path, nodev: bool) -> Self {
            let path = CString::new(path.as_os_str().as_bytes()).unwrap();
            let flags = libc::MS_NOSUID | libc::MS_NOEXEC | if nodev { libc::MS_NODEV } else { 0 };
            // SAFETY: all strings live across mount; each destination is a new
            // test directory, never an existing operator mount.
            let result = unsafe { libc::mount(c"tmpfs".as_ptr(), path.as_ptr(), c"tmpfs".as_ptr(),
                flags, c"size=1m,mode=0700".as_ptr().cast()) };
            assert_eq!(result, 0, "mount: {}", std::io::Error::last_os_error());
            Self(path)
        }
    }
    impl Drop for Mount {
        fn drop(&mut self) {
            // SAFETY: this guard owns precisely the temporary mount above.
            let result = unsafe { libc::umount2(self.0.as_ptr(), libc::MNT_DETACH) };
            assert_eq!(result, 0, "unmount: {}", std::io::Error::last_os_error());
        }
    }
    // SAFETY: geteuid has no inputs and no side effects.
    assert_eq!(unsafe { libc::geteuid() }, 0, "run the ignored suite as root");
    let dir = tempfile::tempdir().unwrap();
    let _mount = Mount::new(dir.path(), true);
    std::fs::write(dir.path().join("file"), b"safe").unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let _nested = Mount::new(&dir.path().join("nested"), true);
    std::fs::write(dir.path().join("nested/secret"), b"other mount").unwrap();
    let _socket = UnixListener::bind(dir.path().join("socket")).unwrap();
    rustix::fs::mknodat(rustix::fs::CWD, dir.path().join("device"), rustix::fs::FileType::CharacterDevice,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR, rustix::fs::makedev(1, 3)).unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), context().record().employee, File::open(dir.path()).unwrap()).unwrap();
    assert!(roots.open_read(&context(), "work", "file").is_ok());
    assert!(roots.open_directory(&context(), "work", None).is_ok());
    for path in ["nested/secret", "device", "socket"] {
        assert!(roots.open_read(&context(), "work", path).is_err(), "{path}");
    }
    assert!(roots.open_directory(&context(), "work", Some("nested")).is_err());
    let unsafe_dir = tempfile::tempdir().unwrap();
    let _unsafe_mount = Mount::new(unsafe_dir.path(), false);
    std::fs::write(unsafe_dir.path().join("file"), b"safe bytes on unsafe mount").unwrap();
    let mut unsafe_roots = ResourceRoots::default();
    unsafe_roots.insert("work".into(), context().record().employee, File::open(unsafe_dir.path()).unwrap()).unwrap();
    assert!(unsafe_roots.open_read(&context(), "work", "file").is_err());
    assert!(unsafe_roots.open_directory(&context(), "work", None).is_err());
}

#[test]
fn wiki_roots_cannot_be_authorized_by_workspace_or_another_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert_wiki("agent:main".into(), context().record().employee, directory(dir.path())).unwrap();
    assert!(roots.open_wiki_read(&context(), "agent:main", "page.md").is_err());
    let mut record = context().record().clone();
    record.resources = List::new(vec![ResourceGrant::Wiki { namespace: Text::new("agent:other".into()).unwrap() }]).unwrap();
    assert!(roots.open_wiki_read(&RequestContext::from_verified_record(record), "agent:main", "page.md").is_err());
    assert!(roots.open_read(&context(), "agent:main", "page.md").is_err());
    assert!(roots.insert_wiki("agent:main".into(), context().record().employee, directory(dir.path())).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn wiki_read_uses_openat2_fd_and_never_reopens_swapped_path() {
    use std::{io::Read, os::unix::fs::symlink};
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::write(dir.path().join("page.md"), b"published").unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert_wiki("agent:main".into(), context().record().employee, directory(dir.path())).unwrap();
    let mut record = context().record().clone();
    record.resources = List::new(vec![ResourceGrant::Wiki { namespace: Text::new("agent:main".into()).unwrap() }]).unwrap();
    let context = RequestContext::from_verified_record(record);
    let mut opened = roots.open_wiki_read(&context, "agent:main", "page.md").unwrap();
    std::fs::rename(dir.path().join("page.md"), dir.path().join("saved.md")).unwrap();
    symlink("/etc/passwd", dir.path().join("page.md")).unwrap();
    let mut text = String::new();
    opened.read_to_string(&mut text).unwrap();
    assert_eq!(text, "published");
    assert!(roots.open_wiki_read(&context, "agent:main", "page.md").is_err());
    assert!(roots.open_wiki_read(&context, "agent:main", "../page.md").is_err());
}

#[cfg(unix)]
#[test]
fn wiki_root_replacement_cannot_serve_withdrawn_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("wiki");
    std::fs::create_dir(&live).unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert_wiki("agent:main".into(), context().record().employee, directory(&live)).unwrap();
    assert!(roots.verify_wiki_root("agent:main", &directory(&live)).is_ok());
    std::fs::rename(&live, dir.path().join("withdrawn")).unwrap();
    std::fs::create_dir(&live).unwrap();
    assert!(roots.verify_wiki_root("agent:main", &directory(&live)).is_err());
}

struct TestLive;
impl LiveRequestVerifier for TestLive {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
fn selected_call(workspace: &str, path: &str) -> crate::tool_policy::AuthorizedToolCall {
    struct Selected(String);
    impl crate::tool_policy::ResourceResolver for Selected {
        fn resolve(&self, context: &RequestContext, _: &crate::tool_policy::ToolPolicy, _: &serde_json::Value)
            -> Result<Vec<ResourceGrant>, VerificationError> {
            Ok(vec![ResourceGrant::Employee { id: context.record().employee },
                ResourceGrant::Workspace { handle: Text::new(self.0.clone()).unwrap() }])
        }
    }
    let mut record = context().record().clone();
    record.tools = List::new(vec![Text::new("skill_security_scan".into()).unwrap()]).unwrap();
    record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
        ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() },
        ResourceGrant::Workspace { handle: Text::new("other".into()).unwrap() }]).unwrap();
    record.expires_at = u64::MAX;
    crate::tool_policy::authorize(&RequestContext::with_live_verifier(record, std::sync::Arc::new(TestLive)), "skill_security_scan",
        &serde_json::json!({"path":path}), &Selected(workspace.into())).unwrap()
}

#[test]
fn authorized_tokens_are_bound_to_call_selection_and_file_inode() {
    let work = selected_call("work", "input.txt");
    let other = selected_call("other", "input.txt");
    let other_path = selected_call("work", "another.txt");
    let dir = tempfile::tempdir().unwrap();
    let mut roots = ResourceRoots::default();
    for name in ["work", "other"] {
        roots.insert(name.into(), work.context().record().employee, directory(dir.path())).unwrap();
    }
    assert!(roots.authorize_workspace(&work, "other").is_err());
    let workspace = roots.authorize_workspace(&work, "work").unwrap();
    assert!(workspace.verify(&work).is_ok());
    assert!(workspace.verify(&other).is_err());
    assert!(workspace.verify(&other_path).is_err());
    assert!(workspace.open_file("another.txt").is_err());
    let mut file = AuthorizedFile::for_test(&work, tempfile::tempfile().unwrap());
    assert!(file.verify(&work).is_ok());
    assert!(file.verify(&other).is_err());
    assert!(file.verify(&other_path).is_err());
    // A same-sized regular file is insufficient: it must be this inode.
    file.file = tempfile::tempfile().unwrap();
    assert!(file.verify(&work).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn authorized_file_open_retains_selected_inode_after_path_swap() {
    let call = selected_call("work", "input.txt");
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::write(dir.path().join("input.txt"), b"selected").unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), call.context().record().employee, directory(dir.path())).unwrap();
    let workspace = roots.authorize_workspace(&call, "work").unwrap();
    let mut file = workspace.open_file("input.txt").unwrap();
    std::fs::rename(dir.path().join("input.txt"), dir.path().join("saved.txt")).unwrap();
    std::fs::write(dir.path().join("input.txt"), b"swapped!").unwrap();
    assert!(file.verify(&call).is_ok());
    use std::io::Read;
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "selected");
}

#[test]
fn overbroad_resolver_cannot_replace_explicit_or_ambiguous_workspace_selection() {
    struct Overbroad;
    impl crate::tool_policy::ResourceResolver for Overbroad {
        fn resolve(&self, context: &RequestContext, _: &crate::tool_policy::ToolPolicy, _: &serde_json::Value)
            -> Result<Vec<ResourceGrant>, VerificationError> {
            Ok(context.record().resources.as_slice().to_vec())
        }
    }
    let baseline = selected_call("work", "input.txt");
    let dir = tempfile::tempdir().unwrap();
    let mut roots = ResourceRoots::default();
    for name in ["work", "other"] {
        roots.insert(name.into(), baseline.context().record().employee, directory(dir.path())).unwrap();
    }
    for key in ["workspace", "workspace_id"] {
        let arguments = serde_json::json!({(key):"work", "path":"input.txt"});
        let call = crate::tool_policy::authorize(baseline.context(), "skill_security_scan",
            &arguments, &Overbroad).unwrap();
        assert!(roots.authorize_workspace(&call, "work").is_ok());
        // Both are capability grants and resolver results; the arguments pick work.
        assert!(roots.authorize_workspace(&call, "other").is_err());
    }
    for arguments in [serde_json::json!({"path":"input.txt"}),
        serde_json::json!({"workspace":"work", "workspace_id":"other", "path":"input.txt"})] {
        let call = crate::tool_policy::authorize(baseline.context(), "skill_security_scan",
            &arguments, &Overbroad).unwrap();
        assert!(roots.authorize_workspace(&call, "work").is_err());
        assert!(roots.authorize_workspace(&call, "other").is_err());
    }
    // An implicit target is valid only when resolution selected one workspace,
    // even though the capability itself still grants both work and other.
    assert!(roots.authorize_workspace(&baseline, "work").is_ok());
}

#[test]
fn host_snapshots_bind_selected_origin_call_and_operation() {
    use crate::tool_policy::{authorize, ResourceResolver, ToolPolicy};
    use duduclaw_isolation::worker::ParseOperation;
    use serde_json::{Value, json};
    struct Live;
    impl LiveRequestVerifier for Live {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
    }
    struct Selected(Vec<ResourceGrant>);
    impl ResourceResolver for Selected {
        fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
            Ok(self.0.clone())
        }
    }
    let mut record = context().record().clone();
    record.expires_at = u64::MAX - 1;
    record.tools = List::new(vec![Text::new("web_extract".into()).unwrap()]).unwrap();
    let employee = ResourceGrant::Employee { id: record.employee };
    let origin = ResourceGrant::UrlOrigin { origin: Text::new("https://example.com".into()).unwrap() };
    let other = ResourceGrant::UrlOrigin { origin: Text::new("https://other.example".into()).unwrap() };
    record.resources = List::new(vec![employee.clone(), origin.clone(), other]).unwrap();
    let context = RequestContext::with_live_verifier(record, std::sync::Arc::new(Live));
    let resolver = Selected(vec![employee, origin]);
    let call = authorize(&context, "web_extract", &json!({"url":"https://example.com/a"}), &resolver).unwrap();
    let token = AuthorizedFile::from_host_snapshot(&call, HostSnapshotSource::WebFetch { url: "https://example.com/a" }, b"real body").unwrap();
    assert!(token.verify(&call).is_ok());
    assert!(token.verify_operation(ParseOperation::ExtractHtml).is_ok());
    assert!(token.verify_operation(ParseOperation::InspectAudio).is_err());
    let next = authorize(&context, "web_extract", &json!({"url":"https://example.com/b"}), &resolver).unwrap();
    assert!(token.verify(&next).is_err());
    assert!(AuthorizedFile::from_host_snapshot(&call,
        HostSnapshotSource::WebFetch { url: "https://other.example/a" }, b"wrong").is_err());
    assert!(AuthorizedFile::from_host_snapshot(&call, HostSnapshotSource::InlineAudio, b"wrong").is_err());
    assert!(AuthorizedFile::from_host_snapshot(&call,
        HostSnapshotSource::WebFetch { url: "https://example.com/a" }, &vec![0; 5 * 1024 * 1024 + 1]).is_err());
}

// ── H7 PublicOrigins (D-H2) ────────────────────────────────────────────────
#[test]
fn public_origin_url_resolves_to_the_exact_authorized_origin() {
    use crate::tool_policy::{authorize, ResourceResolver, ToolPolicy};
    use duduclaw_isolation::worker::ParseOperation;
    use serde_json::{Value, json};
    struct Live;
    impl LiveRequestVerifier for Live {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
    }
    struct Unused;
    impl ResourceResolver for Unused {
        fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
            panic!("PublicOrigins calls select only signed grants")
        }
    }
    assert_eq!(public_url_origin("https://Docs.Example.org/a?b#c").unwrap().as_str(), "https://docs.example.org");
    assert_eq!(public_url_origin("https://docs.example.org:443/").unwrap().as_str(), "https://docs.example.org");
    for url in ["http://docs.example.org/", "https://docs.example.org:8443/", "https://u@docs.example.org/",
        "https://10.0.0.1/", "https://[fd00::1]/", "https://0177.0.0.1/", "https://localhost/", "",
        "https://docs.example.org/\t", "file:///etc/passwd"] {
        assert!(public_url_origin(url).is_err(), "{url:?}");
    }
    let grant = ResourceGrant::PublicOrigins { origins: List::new(vec![PublicOrigin::parse("https://example.com").unwrap()]).unwrap() };
    let mut record = context().record().clone();
    record.expires_at = u64::MAX - 1;
    record.tools = List::new(vec![Text::new("web_extract".into()).unwrap()]).unwrap();
    record.resources = List::new(vec![ResourceGrant::EmployeeScope { id: record.employee }, grant.clone()]).unwrap();
    validate_grants(&record).unwrap();
    let context = RequestContext::with_live_verifier(record, std::sync::Arc::new(Live));
    let call = authorize(&context, "web_extract", &json!({"url":"https://example.com/a","selector":"p"}), &Unused).unwrap();
    let authorized = authorize_public_url(&call).unwrap();
    assert_eq!((authorized.url(), authorized.origin().as_str(), authorized.host()),
        ("https://example.com/a", "https://example.com", "example.com"));
    assert!(authorized.verify(&call).is_ok());
    let token = AuthorizedFile::from_host_snapshot(&call, HostSnapshotSource::WebFetch { url: "https://example.com/a" }, b"body").unwrap();
    assert!(token.verify(&call).is_ok());
    assert!(token.verify_operation(ParseOperation::ExtractHtml).is_ok());
    // Another URL of the same origin is a different call; another origin is refused.
    let next = authorize(&context, "web_extract", &json!({"url":"https://example.com/b","selector":"p"}), &Unused).unwrap();
    assert!(authorized.verify(&next).is_err() && token.verify(&next).is_err());
    assert!(AuthorizedFile::from_host_snapshot(&call, HostSnapshotSource::WebFetch { url: "https://example.com/b" }, b"x").is_err());
    assert!(authorize(&context, "web_extract", &json!({"url":"https://other.example/a","selector":"p"}), &Unused).is_err());
    // The PublicOrigins path does not extend to non-web snapshots.
    assert!(AuthorizedFile::from_host_snapshot(&call, HostSnapshotSource::InlineAudio, b"x").is_err());
    assert!(PublicOrigin::parse("https://example.com").unwrap().permits(&public_url_origin("https://example.com/").unwrap()));
}

// ── H6 brokered employee-work root ─────────────────────────────────────────
// Linux only since H5: the broker hands out sealed memfd snapshots.
#[cfg(target_os = "linux")]
mod h6 {
    use super::*;
    use crate::resources::test_broker::TestWorkBroker;
    use serde_json::json;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    pub(super) const RUN: RunId = RunId([3; 16]);
    pub(super) const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);

    struct Toggle(Arc<AtomicBool>);
    impl LiveRequestVerifier for Toggle {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
            if self.0.load(Ordering::Acquire) { Ok(()) } else { Err(VerificationError::Revoked) }
        }
    }
    fn record(run: RunId) -> CapabilityRecord {
        let mut record = context().record().clone();
        record.run = run;
        record.tools = List::new(vec![Text::new("file_read".into()).unwrap(), Text::new("csv_read".into()).unwrap()]).unwrap();
        record.resources = List::new(vec![ResourceGrant::Workspace { handle: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap() }]).unwrap();
        record.expires_at = u64::MAX;
        record
    }
    pub(super) fn live(run: RunId) -> (RequestContext, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(true));
        (RequestContext::with_live_verifier(record(run), Arc::new(Toggle(flag.clone()))), flag)
    }
    pub(super) fn setup(broker: TestWorkBroker) -> (ResourceRoots, Arc<TestWorkBroker>) {
        let broker = Arc::new(broker);
        let mut roots = ResourceRoots::default();
        roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, broker.clone()).unwrap();
        (roots, broker)
    }
    fn read(roots: &ResourceRoots, context: &RequestContext, tool: &str, path: &str)
        -> Result<(crate::tool_policy::AuthorizedToolCall, AuthorizedFile), VerificationError> {
        roots.authorize_file(context, tool, &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": path}))
    }
    fn contents(mut file: AuthorizedFile) -> std::io::Result<String> {
        use std::io::Read;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    #[test]
    fn h6_brokered_root_requires_live_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("notes/a.txt"), b"brokered").unwrap();
        std::fs::write(dir.path().join("t.csv"), b"a,b\n1,2\n").unwrap();
        let (roots, broker) = setup(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
        let (context, flag) = live(RUN);

        // The live run reads through the broker; run and employee come from the capability.
        let (call, file) = read(&roots, &context, "file_read", "notes/a.txt").unwrap();
        assert!(file.verify(&call).is_ok());
        assert_eq!(broker.calls(), vec![(RUN, EMPLOYEE, "notes/a.txt".to_owned())]);
        assert_eq!(contents(file).unwrap(), "brokered");
        assert!(read(&roots, &context, "csv_read", "t.csv").is_ok());
        assert_eq!(broker.calls().len(), 2);

        // An open descriptor stops serving once the run is no longer live.
        let (_, file) = read(&roots, &context, "file_read", "notes/a.txt").unwrap();
        flag.store(false, Ordering::Release);
        assert!(contents(file).is_err());
        // A capability whose run is no longer live never reaches root.
        assert!(read(&roots, &context, "file_read", "notes/a.txt").is_err());
        let consulted = broker.calls().len();
        // Record-only (no live authority) and expired capabilities never reach root.
        let record_only = RequestContext::from_verified_record(record(RUN));
        assert!(matches!(read(&roots, &record_only, "file_read", "notes/a.txt").err(), Some(VerificationError::Unavailable)));
        let mut expired = record(RUN);
        expired.expires_at = 2;
        let expired = RequestContext::with_live_verifier(expired, Arc::new(TestLive));
        assert!(read(&roots, &expired, "file_read", "notes/a.txt").is_err());
        // Another employee's capability, or one without the employee-work grant.
        let mut foreign = record(RUN);
        foreign.employee = EmployeeId([9; 16]);
        let foreign = RequestContext::with_live_verifier(foreign, Arc::new(TestLive));
        assert!(read(&roots, &foreign, "file_read", "notes/a.txt").is_err());
        let mut ungranted = record(RUN);
        ungranted.resources = List::new(vec![ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() }]).unwrap();
        let ungranted = RequestContext::with_live_verifier(ungranted, Arc::new(TestLive));
        assert!(read(&roots, &ungranted, "file_read", "notes/a.txt").is_err());
        assert_eq!(broker.calls().len(), consulted);

        // A capability for another run is passed through as that run; root refuses it.
        let (other_run, _) = live(RunId([4; 16]));
        assert!(matches!(read(&roots, &other_run, "file_read", "notes/a.txt").err(), Some(VerificationError::Unavailable)));
        assert_eq!(broker.calls().last().unwrap().0, RunId([4; 16]));
        // After the run exits root refuses even a still-valid capability.
        let (context, _) = live(RUN);
        broker.end_run();
        assert!(matches!(read(&roots, &context, "file_read", "notes/a.txt").err(), Some(VerificationError::Unavailable)));
    }

    #[test]
    fn h6_path_from_args_normalized_and_bounded() {
        for (path, ok) in WORK_PATH_CONFORMANCE {
            assert_eq!(WorkRelativePath::parse(path).is_ok(), *ok, "{path:?}");
        }
        let component = "a".repeat(WORK_PATH_MAX_COMPONENT_BYTES);
        assert!(WorkRelativePath::parse(&component).is_ok());
        assert!(WorkRelativePath::parse(&format!("{component}a")).is_err());
        let deep = vec!["d"; WORK_PATH_MAX_DEPTH];
        assert!(WorkRelativePath::parse(&deep.join("/")).is_ok());
        assert!(WorkRelativePath::parse(&format!("{}/d", deep.join("/"))).is_err());
        let body = vec!["b".repeat(100); 40].join("/"); // 4039 bytes, 40 components
        assert!(WorkRelativePath::parse(&format!("{body}/{}", "x".repeat(56))).is_ok()); // 4096
        assert!(WorkRelativePath::parse(&format!("{body}/{}", "x".repeat(57))).is_err()); // 4097
        // Never rewritten: the exact argument bytes are what root opens.
        assert_eq!(WorkRelativePath::parse("notes/員工.md").unwrap().as_str(), "notes/員工.md");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("in.txt"), b"x").unwrap();
        let (roots, broker) = setup(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
        let (context, _) = live(RUN);
        let long = format!("{component}a");
        for path in ["../in.txt", "/in.txt", "a//in.txt", "./in.txt", "in.txt/", "a\nb", "a\\in.txt", long.as_str()] {
            assert!(read(&roots, &context, "file_read", path).is_err(), "{path:?}");
        }
        // Identity is never taken from arguments.
        for key in ["run_id", "employee_id", "root", "cwd"] {
            let arguments = json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "in.txt", (key): "x"});
            assert!(roots.authorize_file(&context, "file_read", &arguments).is_err(), "{key}");
        }
        assert!(broker.calls().is_empty());
        // The opened path must be the one this call selected.
        let (call, _) = read(&roots, &context, "file_read", "in.txt").unwrap();
        assert!(matches!(roots.authorize_brokered_file(&call, EMPLOYEE_WORK_ROOT, "other.txt").err(),
            Some(VerificationError::BindingMismatch)));
        assert!(roots.authorize_brokered_file(&call, "work", "in.txt").is_err());
        assert_eq!(broker.calls(), vec![(RUN, EMPLOYEE, "in.txt".to_owned())]);
    }

    #[test]
    fn h6_no_host_path_fallback() {
        use crate::tool_policy::{authorize, ResourceResolver, ToolPolicy};
        struct Selected;
        impl ResourceResolver for Selected {
            fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &serde_json::Value)
                -> Result<Vec<ResourceGrant>, VerificationError> {
                Ok(vec![ResourceGrant::Workspace { handle: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap() }])
            }
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"host bytes").unwrap();
        let (mut roots, broker) = setup(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
        broker.fail_with(Some(WorkBrokerError::Unavailable));
        let (context, _) = live(RUN);
        assert!(matches!(read(&roots, &context, "file_read", "a.txt").err(), Some(VerificationError::Unavailable)));
        // No gateway dirfd exists for the brokered handle: every pathname route refuses.
        assert!(roots.open_read(&context, EMPLOYEE_WORK_ROOT, "a.txt").is_err());
        assert!(roots.open_directory(&context, EMPLOYEE_WORK_ROOT, None).is_err());
        let call = authorize(&context, "file_read", &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "a.txt"}), &Selected).unwrap();
        assert!(roots.authorize_workspace(&call, EMPLOYEE_WORK_ROOT).is_err());
        let policy = duduclaw_isolation::execution::WorkspacePolicy { workspace: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap(),
            relative_cwd: Text::new(".".into()).unwrap() };
        assert!(roots.authorize_helper_workspace(&context, &policy).is_err());
        // A host dirfd cannot be registered beside (or instead of) the broker.
        assert!(roots.insert(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, directory(dir.path())).is_err());
        assert!(roots.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, broker.clone()).is_err());
        let mut plain = ResourceRoots::default();
        assert!(plain.insert_brokered("work".into(), EMPLOYEE, broker.clone()).is_err());
        plain.insert(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, directory(dir.path())).unwrap();
        assert!(plain.insert_brokered(EMPLOYEE_WORK_ROOT.into(), EMPLOYEE, broker.clone()).is_err());
        // Every failed attempt above went to root only, never to the host file.
        assert!(broker.calls().iter().all(|(run, employee, path)| *run == RUN && *employee == EMPLOYEE && path == "a.txt"));
    }

    #[test]
    fn h6_broker_error_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"data").unwrap();
        let (roots, broker) = setup(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
        let (context, _) = live(RUN);
        for error in [WorkBrokerError::NotLive, WorkBrokerError::NotFound, WorkBrokerError::Refused,
            WorkBrokerError::TooLarge, WorkBrokerError::Unavailable] {
            broker.fail_with(Some(error));
            assert!(matches!(read(&roots, &context, "file_read", "a.txt").err(), Some(VerificationError::Unavailable)), "{error:?}");
        }
        broker.fail_with(None);
        assert!(matches!(read(&roots, &context, "file_read", "missing.txt").err(), Some(VerificationError::Unavailable)));
        assert!(read(&roots, &context, "file_read", "a.txt").is_ok());
        // A descriptor that is not the inode root reported is refused.
        broker.misreport(|metadata| metadata.inode ^= 1);
        assert!(matches!(read(&roots, &context, "file_read", "a.txt").err(), Some(VerificationError::Unavailable)));
        broker.misreport(|metadata| metadata.device ^= 1);
        assert!(matches!(read(&roots, &context, "file_read", "a.txt").err(), Some(VerificationError::Unavailable)));
    }

    #[test]
    fn h6_non_regular_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("dir")).unwrap();
        std::fs::write(dir.path().join("file"), b"data").unwrap();
        std::fs::write(dir.path().join("single"), b"data").unwrap();
        std::fs::hard_link(dir.path().join("file"), dir.path().join("hard")).unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.path().join("null")).unwrap();
        std::os::unix::fs::symlink("file", dir.path().join("link")).unwrap();
        std::fs::File::create(dir.path().join("limit")).unwrap().set_len(BROKERED_FILE_MAX_BYTES).unwrap();
        std::fs::File::create(dir.path().join("big")).unwrap().set_len(BROKERED_FILE_MAX_BYTES + 1).unwrap();
        let (context, _) = live(RUN);

        // A correct broker refuses them itself (fail closed as Unavailable).
        let (roots, _) = setup(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
        for path in ["dir", "hard", "null", "link", "big"] {
            assert!(matches!(read(&roots, &context, "file_read", path).err(), Some(VerificationError::Unavailable)), "{path}");
        }
        assert!(read(&roots, &context, "file_read", "limit").is_ok());

        // A faulty broker that hands back the live descriptor anyway is still
        // refused, whether it reports the truth or claims a sealed snapshot's
        // shape (regular, nlink 0, small). Since H5 that includes the plain
        // single-link file: only a sealed memfd snapshot is accepted.
        let (roots, broker) = setup(TestWorkBroker::unchecked(dir.path().to_owned(), RUN, EMPLOYEE));
        for lie in [false, true] {
            if lie { broker.misreport(|metadata| { metadata.regular = true; metadata.nlink = 0; metadata.size = 0; }); }
            for path in ["dir", "hard", "null", "big", "single"] {
                assert!(matches!(read(&roots, &context, "file_read", path).err(), Some(VerificationError::Invalid)), "{path} lie={lie}");
            }
        }
        // A broker-reported non-regular, linked or oversized snapshot is refused before use.
        let (roots, broker) = setup(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
        assert!(read(&roots, &context, "file_read", "single").is_ok());
        broker.misreport(|metadata| metadata.regular = false);
        assert!(matches!(read(&roots, &context, "file_read", "single").err(), Some(VerificationError::Invalid)));
        for nlink in [1, 2] {
            broker.misreport(move |metadata| metadata.nlink = nlink);
            assert!(matches!(read(&roots, &context, "file_read", "single").err(), Some(VerificationError::Invalid)), "{nlink}");
        }
        broker.misreport(|metadata| metadata.size = BROKERED_FILE_MAX_BYTES + 1);
        assert!(matches!(read(&roots, &context, "file_read", "single").err(), Some(VerificationError::Invalid)));
    }
}

// ── H5 sealed snapshot + file-tool refusals ────────────────────────────────
#[cfg(target_os = "linux")]
mod h5 {
    use super::*;
    use super::h6::{live, setup, EMPLOYEE, RUN};
    use crate::resources::test_broker::{sealed_snapshot, TestWorkBroker};
    use serde_json::json;
    use std::{io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::{MetadataExt, OpenOptionsExt}}};
    type Edit = fn(&mut WorkFileMetadata);

    fn claim(file: &File) -> WorkFileMetadata {
        let stat = file.metadata().unwrap();
        WorkFileMetadata { regular: stat.is_file(), size: stat.len(), device: stat.dev(), inode: stat.ino(), nlink: stat.nlink() }
    }
    fn verify(file: File) -> Result<File, VerificationError> {
        let metadata = claim(&file);
        verify_brokered(BrokeredWorkFile { file, metadata })
    }
    /// A writable memfd with exactly `seals` added (0 = none).
    fn memfd(bytes: &[u8], seals: i32) -> File {
        let raw = unsafe { libc::memfd_create(c"h5-test".as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
        assert!(raw >= 0);
        let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        file.write_all(bytes).unwrap();
        if seals != 0 { assert_eq!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) }, 0); }
        file
    }
    fn read_only(file: &File) -> File { File::open(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap() }
    const ALL: i32 = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;

    #[test]
    fn h5_verify_brokered_accepts_sealed_memfd() {
        let dir = tempfile::tempdir().unwrap();
        for bytes in [b"snapshot bytes".to_vec(), Vec::new(), vec![7u8; BROKERED_FILE_MAX_BYTES as usize]] {
            std::fs::write(dir.path().join("source"), &bytes).unwrap();
            let snapshot = sealed_snapshot(&mut File::open(dir.path().join("source")).unwrap()).unwrap();
            let metadata = claim(&snapshot);
            assert_eq!((metadata.regular, metadata.nlink, metadata.size), (true, 0, bytes.len() as u64));
            let mut file = verify_brokered(BrokeredWorkFile { file: snapshot, metadata }).unwrap();
            let mut read = Vec::new();
            file.read_to_end(&mut read).unwrap();
            assert_eq!(read, bytes);
            // Immutable and read-only: the received fd can neither write nor resize.
            assert!(file.write_all(b"x").is_err());
            assert!(file.set_len(0).is_err());
        }
        // The snapshot is a copy: rewriting the source afterwards changes nothing.
        std::fs::write(dir.path().join("source"), b"before").unwrap();
        let snapshot = sealed_snapshot(&mut File::open(dir.path().join("source")).unwrap()).unwrap();
        std::fs::write(dir.path().join("source"), b"after!").unwrap();
        let mut text = String::new();
        verify(snapshot).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "before");
        // A hand-built O_RDONLY reopen of a fully sealed memfd is equally fine.
        assert!(verify(read_only(&memfd(b"x", ALL))).is_ok());
        // The claim must be honest: the old nlink-1 claim, or another inode/size.
        let snapshot = read_only(&memfd(b"xyz", ALL));
        let invalid: [Edit; 3] = [|m: &mut WorkFileMetadata| m.nlink = 1, |m: &mut WorkFileMetadata| m.regular = false,
            |m: &mut WorkFileMetadata| m.size = BROKERED_FILE_MAX_BYTES + 1];
        for edit in invalid {
            let mut metadata = claim(&snapshot);
            edit(&mut metadata);
            let file = snapshot.try_clone().unwrap();
            assert_eq!(verify_brokered(BrokeredWorkFile { file, metadata }).err(), Some(VerificationError::Invalid));
        }
        let mismatched: [Edit; 3] = [|m: &mut WorkFileMetadata| m.inode ^= 1, |m: &mut WorkFileMetadata| m.device ^= 1,
            |m: &mut WorkFileMetadata| m.size = 2];
        for edit in mismatched {
            let mut metadata = claim(&snapshot);
            edit(&mut metadata);
            let file = snapshot.try_clone().unwrap();
            assert_eq!(verify_brokered(BrokeredWorkFile { file, metadata }).err(), Some(VerificationError::Unavailable));
        }
    }

    #[test]
    fn h5_verify_brokered_refuses_unsealed_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain"), b"live").unwrap();
        // The employee's live inode (nlink 1), honestly reported or claimed as nlink 0.
        let plain = File::open(dir.path().join("plain")).unwrap();
        assert_eq!(verify(plain.try_clone().unwrap()).err(), Some(VerificationError::Invalid));
        let mut lie = claim(&plain);
        lie.nlink = 0;
        assert_eq!(verify_brokered(BrokeredWorkFile { file: plain, metadata: lie }).err(), Some(VerificationError::Invalid));
        // An unlinked regular file (nlink 0, but no seals).
        let unlinked = File::open(dir.path().join("plain")).unwrap();
        std::fs::remove_file(dir.path().join("plain")).unwrap();
        assert_eq!(unlinked.metadata().unwrap().nlink(), 0);
        assert_eq!(verify(unlinked).err(), Some(VerificationError::Invalid));
        // memfds that are not fully sealed: none, missing one seal each.
        for seals in [0, ALL & !libc::F_SEAL_WRITE, ALL & !libc::F_SEAL_GROW, ALL & !libc::F_SEAL_SHRINK, ALL & !libc::F_SEAL_SEAL] {
            assert_eq!(verify(read_only(&memfd(b"x", seals))).err(), Some(VerificationError::Invalid), "{seals:#x}");
        }
        // A fully sealed memfd on a writable description.
        assert_eq!(verify(memfd(b"x", ALL)).err(), Some(VerificationError::Invalid));
        // An unlinked O_TMPFILE on tmpfs: nlink 0 and F_GET_SEALS works, but it
        // starts with F_SEAL_SEAL only and can never be write-sealed.
        if let Ok(tmpfile) = std::fs::OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_TMPFILE).open("/dev/shm") {
            let mut tmpfile = tmpfile;
            tmpfile.write_all(b"x").unwrap();
            assert_eq!(tmpfile.metadata().unwrap().nlink(), 0);
            assert_eq!(verify(read_only(&tmpfile)).err(), Some(VerificationError::Invalid));
        }
        // Through the boundary: the unchecked broker's live fd never authorizes a read.
        std::fs::write(dir.path().join("a.txt"), b"live").unwrap();
        let (roots, _) = setup(TestWorkBroker::unchecked(dir.path().to_owned(), RUN, EMPLOYEE));
        let (context, _) = live(RUN);
        assert!(matches!(roots.authorize_file(&context, "file_read", &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "a.txt"})).err(),
            Some(VerificationError::Invalid)));
    }

    #[test]
    fn h5_error_mapping_no_path_leak() {
        let all = [WorkBrokerError::NotLive, WorkBrokerError::NotFound, WorkBrokerError::Refused,
            WorkBrokerError::TooLarge, WorkBrokerError::Unavailable];
        let messages: std::collections::BTreeSet<_> = all.iter().map(|error| error.user_message()).collect();
        assert_eq!(messages.len(), all.len(), "each refusal has its own message");
        assert_eq!(WorkBrokerError::Unavailable.code(), "workspace_broker_unavailable");
        assert_eq!(WorkBrokerError::NotFound.code(), "work_file_not_found");
        assert_eq!(WorkBrokerError::Refused.code(), "work_file_refused");
        assert_eq!(WorkBrokerError::TooLarge.code(), "work_file_too_large");

        let dir = tempfile::tempdir().unwrap();
        let secret = "機密資料夾/薪資-2026.xlsx";
        std::fs::create_dir(dir.path().join("機密資料夾")).unwrap();
        std::fs::write(dir.path().join("target.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("../target.txt", dir.path().join("機密資料夾/捷徑-2026.xlsx")).unwrap();
        std::fs::File::create(dir.path().join("機密資料夾/巨大-2026.xlsx")).unwrap().set_len(BROKERED_FILE_MAX_BYTES + 1).unwrap();
        let (roots, broker) = setup(TestWorkBroker::new(dir.path().to_owned(), RUN, EMPLOYEE));
        let (context, _) = live(RUN);
        let refusal = |path: &str| roots.authorize_file_with_refusal(&context, "file_read",
            &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": path})).err().unwrap();
        let mut seen = Vec::new();
        for (path, expected) in [(secret, WorkBrokerError::NotFound),
            ("機密資料夾/捷徑-2026.xlsx", WorkBrokerError::Refused), ("機密資料夾/巨大-2026.xlsx", WorkBrokerError::TooLarge)] {
            let refusal = refusal(path);
            assert_eq!(refusal.broker(), Some(expected), "{path}");
            // The boundary contract is unchanged: every broker refusal is Unavailable.
            assert_eq!(refusal.verification(), VerificationError::Unavailable);
            assert!(matches!(roots.authorize_file(&context, "file_read", &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": path})).err(),
                Some(VerificationError::Unavailable)));
            seen.push((path, refusal));
        }
        for error in [WorkBrokerError::Unavailable, WorkBrokerError::NotLive] {
            broker.fail_with(Some(error));
            let refusal = refusal(secret);
            assert_eq!(refusal.broker(), Some(error));
            seen.push((secret, refusal));
        }
        broker.fail_with(None);
        // A boundary refusal (bad path) is not a broker refusal and never reaches root.
        let calls = broker.calls().len();
        assert!(matches!(refusal("../機密資料夾/薪資-2026.xlsx"), FileRefusal::Verification(_)));
        assert_eq!(broker.calls().len(), calls);

        let host = dir.path().to_string_lossy().into_owned();
        for (path, refusal) in seen {
            let message = refusal.broker().unwrap().user_message();
            for fragment in path.split('/').chain(path.split(['/', '-', '.'])).filter(|part| !part.is_empty()) {
                assert!(!message.contains(fragment), "{message} leaks {fragment:?}");
            }
            assert!(!message.contains(&host) && !message.contains(EMPLOYEE_WORK_ROOT) && !message.contains('/'), "{message}");
            assert!(!format!("{refusal:?}").contains("2026"));
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn helper_workspace_token_binds_owner_run_and_relative_directory() {
    use duduclaw_isolation::execution::WorkspacePolicy;
    let directory = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::create_dir(directory.path().join("notes")).unwrap();
    let context = context();
    let mut roots = ResourceRoots::default();
    roots.insert("work".into(), context.record().employee, File::open(directory.path()).unwrap()).unwrap();
    let policy = WorkspacePolicy { workspace: Text::new("work".into()).unwrap(),
        relative_cwd: Text::new("notes".into()).unwrap() };
    let proof = roots.authorize_helper_workspace(&context, &policy).unwrap();
    assert!(proof.permits(&context, &policy));
    let child = duduclaw_isolation::execution::RunHandle(RunId([9; 16]));
    let delivery = proof.delivery_for(child).unwrap();
    assert_eq!(delivery.run, child.0);
    assert_ne!(delivery.run, context.record().run);
    assert_eq!(delivery.employee, context.record().employee);
    assert_eq!(delivery.policy, policy);
    assert_eq!(file_identity(&delivery.directory).unwrap(), file_identity(&File::open(directory.path().join("notes")).unwrap()).unwrap());
    // Producing child delivery does not rebind the source authorization proof.
    assert!(proof.permits(&context, &policy));
    let mut other_run = context.record().clone();
    other_run.run = child.0;
    assert!(!proof.permits(&RequestContext::from_verified_record(other_run), &policy));
    assert!(proof.delivery_for(duduclaw_isolation::execution::RunHandle(context.record().run)).is_err());
    let mut wrong = policy.clone();
    wrong.relative_cwd = Text::new("../private".into()).unwrap();
    assert!(!proof.permits(&context, &wrong));
    assert!(roots.authorize_helper_workspace(&context, &wrong).is_err());
    std::os::unix::fs::symlink("notes", directory.path().join("link")).unwrap();
    wrong.relative_cwd = Text::new("link".into()).unwrap();
    assert!(roots.authorize_helper_workspace(&context, &wrong).is_err());
    wrong.workspace = Text::new("unregistered".into()).unwrap();
    assert!(roots.authorize_helper_workspace(&context, &wrong).is_err());
}
