//! Bounded local reads that never wait for a FIFO writer.
use std::{fs::{File, OpenOptions}, io, path::Path};

pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: inspect the link itself, not its target.
        options.custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a regular file"));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use crate::{read_state::ReadState, settings::{Glyphs, Settings}};

    fn refused(home: &std::path::Path) {
        let (settings, notice) = Settings::load(home);
        assert_eq!(settings.glyphs, Glyphs::Ascii);
        assert!(notice.is_some());
        let (read, notice) = ReadState::load(home);
        assert!(!read.contains("secret"));
        assert!(notice.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_including_dangling_links_are_refused() {
        let home = tempfile::tempdir().unwrap();
        for (name, text) in [("tui.toml", "glyphs = 'unicode'"), ("tui-state.json", "{\"ids\":[\"secret\"]}")] {
            let target = home.path().join(format!("{name}.target"));
            std::fs::write(&target, text).unwrap();
            std::os::unix::fs::symlink(target, home.path().join(name)).unwrap();
        }
        refused(home.path());
        for name in ["tui.toml.target", "tui-state.json.target"] {
            std::fs::remove_file(home.path().join(name)).unwrap();
        }
        refused(home.path());
    }

    #[cfg(unix)]
    #[test]
    #[allow(unsafe_code)] // mkfifo only creates the isolated test fixture.
    fn fifos_return_promptly_without_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let home = tempfile::tempdir().unwrap();
        for name in ["tui.toml", "tui-state.json"] {
            let path = std::ffi::CString::new(home.path().join(name).as_os_str().as_bytes()).unwrap();
            // SAFETY: path is a valid, NUL-terminated string.
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let path = home.path().to_owned();
        let worker = std::thread::spawn(move || { refused(&path); tx.send(()).unwrap(); });
        rx.recv_timeout(std::time::Duration::from_secs(2)).expect("FIFO read blocked");
        worker.join().unwrap();
    }

    #[test]
    fn directories_are_refused() {
        let home = tempfile::tempdir().unwrap();
        for name in ["tui.toml", "tui-state.json"] {
            std::fs::create_dir(home.path().join(name)).unwrap();
        }
        refused(home.path());
    }
}
