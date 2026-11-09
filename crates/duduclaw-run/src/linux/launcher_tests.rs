//! Host-side launcher tests (moved out of `launcher.rs` in the HX6 security
//! fix round to keep that file below 800 lines).
use super::*;
#[test]
fn scratch_listing_parses_getdents_and_cleanup_is_no_follow() {
    // Host-side check of the no-follow removal on a private temporary tree.
    let base = std::env::temp_dir().join(format!("duduclaw-hx3b-scratch-{}", std::process::id()));
    std::fs::create_dir(&base).unwrap();
    let call = base.join("call");
    std::fs::create_dir_all(call.join("a/b/c")).unwrap();
    std::fs::write(call.join("a/b/c/file"), b"x").unwrap();
    std::fs::write(call.join("top"), b"y").unwrap();
    let outside = base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"z").unwrap();
    std::os::unix::fs::symlink(&outside, call.join("link")).unwrap();
    let parent = std::fs::File::open(&base).unwrap();
    let mut names = list_directory(AsRawFd::as_raw_fd(&std::fs::File::open(&call).unwrap()));
    names.sort();
    assert_eq!(names, vec![CString::new("a").unwrap(), CString::new("link").unwrap(), CString::new("top").unwrap()]);
    let mut budget = SCRATCH_BUDGET;
    remove_entry(parent.as_raw_fd(), c"call", 0, &mut budget);
    assert!(!call.exists(), "scratch tree removed");
    assert!(outside.join("keep").exists(), "a symlink inside scratch is unlinked, never followed");
    std::fs::remove_dir_all(&base).unwrap();
}
#[test]
fn helper_child_rlimits_are_hard_equal_soft() {
    // Runs in a forked child: lowering limits is irreversible for that process.
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        let ok = limit(libc::RLIMIT_CORE as u32, 0).is_ok() && limit(libc::RLIMIT_FSIZE as u32, 1 << 20).is_ok()
            && limit(libc::RLIMIT_NOFILE as u32, 64).is_ok();
        let mut nofile: libc::rlimit64 = unsafe { std::mem::zeroed() };
        unsafe { libc::syscall(libc::SYS_prlimit64, 0, libc::RLIMIT_NOFILE as u32, std::ptr::null::<libc::rlimit64>(), &mut nofile); }
        let raise = libc::rlimit64 { rlim_cur: 65, rlim_max: 65 };
        let raised = unsafe { libc::syscall(libc::SYS_prlimit64, 0, libc::RLIMIT_NOFILE as u32, &raise,
            std::ptr::null_mut::<libc::rlimit64>()) } == 0;
        let exact = nofile.rlim_cur == 64 && nofile.rlim_max == 64;
        // Root (CAP_SYS_RESOURCE) may raise; an unprivileged child may not.
        let expect_raise = unsafe { libc::geteuid() } == 0;
        unsafe { libc::_exit(if ok && exact && raised == expect_raise { 0 } else { 1 }); }
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, "{status}");
}
