//! Select by the detached composition syscall result, never a kernel release.
use std::io;

pub(crate) fn needs_attached_composition(result: io::Result<()>) -> io::Result<bool> {
    // Linux EINVAL. Keep this selector platform-independent so host unit tests
    // exercise the policy even when the mount implementation is not compiled.
    const LINUX_EINVAL: i32 = 22;
    match result {
        Ok(()) => Ok(false),
        Err(error) if error.raw_os_error() == Some(LINUX_EINVAL) => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn successful_detached_composition_keeps_fast_path() {
        assert!(!needs_attached_composition(Ok(())).unwrap());
    }
    #[test]
    fn only_linux_einval_selects_attached_composition() {
        assert!(needs_attached_composition(Err(io::Error::from_raw_os_error(22))).unwrap());
        for errno in [1, 2, 5, 9, 12, 13, 16, 28, 38, 95] {
            assert_eq!(needs_attached_composition(Err(io::Error::from_raw_os_error(errno)))
                .unwrap_err().raw_os_error(), Some(errno));
        }
        assert_eq!(needs_attached_composition(Err(io::ErrorKind::InvalidInput.into()))
            .unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
