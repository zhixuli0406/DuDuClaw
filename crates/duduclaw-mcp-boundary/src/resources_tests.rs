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
