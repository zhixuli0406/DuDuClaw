//! Debug-only signed payload for disposable native PTY acceptance.
#[cfg(not(debug_assertions))]
compile_error!("the PTY acceptance fixture must never ship in a release build");

#[cfg(target_os = "linux")]
fn main() {
    use std::{fs::File, io::{Read, Write}, os::fd::FromRawFd};
    assert_eq!(std::env::args_os().count(), 1);
    assert!((70000..=99999).contains(&unsafe { libc::geteuid() }));
    for fd in 0..=2 { assert_eq!(unsafe { libc::isatty(fd) }, 1); }
    assert_eq!(unsafe { libc::tcgetsid(0) }, unsafe { libc::getsid(0) });
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(0, &mut settings) }, 0);
    unsafe { libc::cfmakeraw(&mut settings); }
    assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &settings) }, 0);
    let fd = unsafe { libc::fcntl(4, libc::F_DUPFD_CLOEXEC, 10) }; assert!(fd >= 0);
    let mut prompt = String::new();
    unsafe { File::from_raw_fd(fd) }.take(65).read_to_string(&mut prompt).unwrap();
    assert!(prompt.len() == 32 && prompt.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let mut initial: libc::winsize = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut initial) }, 0);
    println!("ready:{prompt}:{}:{}", initial.ws_row, initial.ws_col); std::io::stdout().flush().unwrap();
    loop {
        let mut input = [0]; std::io::stdin().read_exact(&mut input).unwrap();
        match input[0] {
            b'S' => {
                let mut size: libc::winsize = unsafe { std::mem::zeroed() };
                assert_eq!(unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) }, 0);
                println!("size:{}:{}", size.ws_row, size.ws_col);
            }
            b'P' => println!("pong"),
            b'E' => return,
            _ => panic!("unknown acceptance byte"),
        }
        std::io::stdout().flush().unwrap();
    }
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("PTY acceptance requires native Linux"); }
