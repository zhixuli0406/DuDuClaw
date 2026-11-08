//! HX6 security fix round (finding 5): the launcher plan carries the
//! recording binding's signed browser files (bound under `/helpers` and
//! granted to that helper only); no other binding may carry files, and the
//! nested-sandbox exception is recording-only.
use super::*;
use crate::helper_policy::{HelperFs, HelperSlot, SelfOnlyCall, RECORDING_BROWSER_FILES};

fn program(argv: &[&str], calls: &[u32]) -> Program {
    let arch = Architecture::native().unwrap();
    let seccomp = RuntimeProfile { allowed_syscalls: calls.to_vec() };
    let seccomp_digest = seccomp.digest(arch).unwrap();
    Program { identity: Identity { inode: 7, mount: 9, dev_major: 8, dev_minor: 1 }, script: None,
        argv: argv.iter().map(|arg| CString::new(*arg).unwrap()).collect(), environment: vec![], seccomp, seccomp_digest }
}
fn files(names: &[&str]) -> Vec<(Identity, CString)> {
    names.iter().zip(20u64..).map(|(name, inode)| (Identity { inode, mount: 9, dev_major: 8, dev_minor: 1 },
        CString::new(*name).unwrap())).collect()
}

#[test]
fn hx6fix_launcher_plan_carries_recording_browser_files_only() {
    let arch = Architecture::native().unwrap();
    let socket = match arch { Architecture::X86_64 => 41, Architecture::Aarch64 => 198 };
    let mut parent_calls = arch.launcher_parent_calls().to_vec();
    parent_calls.push(socket);
    let parent = program(&["/runtime/claude"], &parent_calls);
    let helper_calls = [arch.execveat(), arch.exit_group(), socket];
    let recording = |files: Vec<(Identity, CString)>, self_only: Vec<SelfOnlyCall>| HelperProgram {
        program: program(&["/helpers/record", "--browser-record-fd-v1"], &helper_calls), self_only, mount: 0,
        slot: HelperSlot::Recording, filesystem: vec![HelperFs::SystemRo, HelperFs::WorkRecordings],
        open_files: 64, file_size_bytes: 1 << 20, timeout_ms: 30_000, egress: true, files };
    let plan = |helper: HelperProgram| LauncherPlan { helpers: vec![helper], mounts: vec![MountIdentity { mount: 9, fs_type: 1 }],
        tmpfs_bytes: 1 << 20, drain_ms: 10_000 };
    let sandbox = vec![SelfOnlyCall::NestedSandbox];
    let ok = plan(recording(files(&RECORDING_BROWSER_FILES), sandbox.clone()));
    assert!(ok.validate(&parent).is_ok());
    // One descriptor per file after the executable: 1 control + 1 mount + 1 exec + 3 files + 4 cgroup.
    assert_eq!(ok.fd_count(), 10);
    // No files (fails closed later) is a valid plan; any other list is not.
    assert!(plan(recording(vec![], sandbox.clone())).validate(&parent).is_ok());
    for names in [&["node", "playwright.cjs"][..], &["node", "chromium", "playwright.cjs"], &["node", "playwright.cjs", "chromium", "extra"],
        &["node", "playwright.cjs", "../chromium"]] {
        assert_eq!(plan(recording(files(names), sandbox.clone())).validate(&parent), Err(Error::Executable), "{names:?}");
    }
    // A name already bound with another identity (here: the entry itself) is refused.
    let mut clash = files(&RECORDING_BROWSER_FILES);
    clash[0].1 = CString::new("record").unwrap();
    assert!(plan(recording(clash, sandbox.clone())).validate(&parent).is_err());
    // Foreground helpers carry no files and never the nested-sandbox exception.
    let mut foreground = recording(files(&RECORDING_BROWSER_FILES), vec![]);
    foreground.slot = HelperSlot::Foreground; foreground.egress = false; foreground.filesystem = vec![HelperFs::SystemRo, HelperFs::Work];
    foreground.program = program(&["/helpers/program"], &[arch.execveat(), arch.exit_group()]);
    assert_eq!(plan(foreground).validate(&parent), Err(Error::Executable));
    let mut offline = recording(vec![], sandbox);
    offline.slot = HelperSlot::Foreground; offline.egress = false; offline.filesystem = vec![HelperFs::SystemRo, HelperFs::Work];
    offline.program = program(&["/helpers/program"], &[arch.execveat(), arch.exit_group()]);
    assert_eq!(plan(offline).validate(&parent), Err(Error::Seccomp));
    // The signed plan round-trips; an old plan without `files` reads as none.
    let value = serde_json::to_value(&ok.helpers[0]).unwrap();
    let back: HelperProgram = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(back.files.len(), 3);
    let mut old = value; old.as_object_mut().unwrap().remove("files");
    assert!(serde_json::from_value::<HelperProgram>(old).unwrap().files.is_empty());
}
