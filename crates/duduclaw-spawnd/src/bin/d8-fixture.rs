//! Debug-only employee payload for signed, disposable-host D8 acceptance.
//! This binary is not a runtime shipped by production manifests. The test host
//! must explicitly sign/register its exact ELF and restrictive syscall profile.
#[cfg(not(debug_assertions))]
compile_error!("the D8 acceptance fixture must never be included in a release build");

#[cfg(target_os = "linux")]
fn main() {
    use std::{fs::File, io::{Read, Write}, os::fd::{AsRawFd, FromRawFd}, time::{Duration, Instant}};
    assert_eq!(std::env::args_os().count(), 1, "root registry fixes an empty argument list");
    let uid = unsafe { libc::geteuid() };
    assert!((duduclaw_isolation::ids::EMPLOYEE_UID_MIN..=duduclaw_isolation::ids::EMPLOYEE_UID_MAX).contains(&uid));
    assert_eq!(unsafe { libc::getuid() }, uid);
    assert_eq!(unsafe { libc::getgid() }, uid);
    let fd = unsafe { libc::dup(4) };
    assert!(fd >= 0);
    let mut prompt = unsafe { File::from_raw_fd(fd) };
    let seals = unsafe { libc::fcntl(prompt.as_raw_fd(), libc::F_GET_SEALS) };
    assert!(seals >= 0 && seals & 15 == 15);
    let mut bytes = String::new();
    (&mut prompt).take(4096).read_to_string(&mut bytes).unwrap();
    let input: serde_json::Value = serde_json::from_str(&bytes).unwrap();
    let object = input.as_object().unwrap();
    assert_eq!(object.len(), 2);
    let role = input["role"].as_str().unwrap();
    let nonce = input["nonce"].as_str().unwrap();
    assert!(nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(matches!(role, "first" | "second"));
    let name = if role == "first" { c"iso-d8-main" } else { c"iso-d8-second" };
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr()) }, 0);
    if role == "first" {
        let mut ready = [-1; 2];
        assert_eq!(unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe { libc::close(ready[0]); }
            let orphan = unsafe { libc::fork() };
            if orphan < 0 { unsafe { libc::_exit(120); } }
            if orphan > 0 { unsafe { libc::_exit(0); } }
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_NAME, c"iso-d8-orphan".as_ptr()) }, 0);
            assert!(unsafe { libc::setsid() } > 0);
            let deadline = Instant::now() + Duration::from_secs(10);
            while unsafe { libc::getppid() } != 1 {
                assert!(Instant::now() < deadline, "orphan must be adopted by namespace init");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(unsafe { libc::write(ready[1], b"R".as_ptr().cast(), 1) }, 1);
            unsafe { libc::close(ready[1]); }
            // Stay alive beyond main exit. B2's namespace teardown must kill it;
            // root holds init briefly so B1's queue behavior can be observed.
            loop { unsafe { libc::pause(); } }
        }
        unsafe { libc::close(ready[1]); }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        let mut ready = unsafe { File::from_raw_fd(ready[0]) };
        let mut byte = [0; 1];
        ready.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"R");
    }
    println!("{}", serde_json::json!({"ready":role,"nonce":nonce}));
    std::io::stdout().flush().unwrap();
    let mut byte = [0; 1];
    std::io::stdin().read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"E");
}

#[cfg(not(target_os = "linux"))]
fn main() { panic!("the D8 acceptance fixture requires native Linux"); }
