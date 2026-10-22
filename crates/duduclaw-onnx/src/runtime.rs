//! ONNX Runtime bootstrap (`load-dynamic`).

use std::path::Path;
use std::sync::OnceLock;

/// Load `libonnxruntime` from the installed path, once per process.
///
/// With ort's `load-dynamic` feature the binary carries no ONNX Runtime; the
/// library is whatever the install step put under
/// `<home>/lib/onnxruntime/<ver>/`. Initialising twice is an error in ort, so
/// the outcome (including the failure text) is memoised — and shared by every
/// feature in the process (NER redaction, knowledge embeddings). The first
/// caller's path wins; every caller passes the same versioned path.
///
/// A library that is not there yet is reported without being memoised, so
/// installing it later in the same process still works.
pub fn init_ort(lib_path: &Path) -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    if INIT.get().is_none() {
        if !lib_path.exists() {
            return Err(format!("ONNX Runtime 執行庫不存在：{}", lib_path.display()));
        }
        // Checked at the load itself, not only when a feature was opened:
        // a library swapped in between is refused (and not memoised, so a
        // reinstall works without a restart).
        crate::install::verify_ort_lib_path(lib_path)?;
    }
    INIT.get_or_init(|| {
        let builder = ort::init_from(lib_path)
            .map_err(|e| format!("載入 ONNX Runtime 失敗（{}）：{e}", lib_path.display()))?;
        // `commit` returns false when an environment is already installed —
        // harmless (another part of the process got there first), and not a
        // reason to refuse to run.
        let _ = builder.commit();
        Ok(())
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_that_is_not_the_pinned_build_is_refused_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("libonnxruntime.so");
        std::fs::write(&lib, b"swapped in after the install check").unwrap();
        let err = init_ort(&lib).unwrap_err();
        assert!(err.contains("ONNX Runtime"), "{err}");
        // Not memoised: a missing library still reports missing.
        assert!(
            init_ort(&dir.path().join("absent"))
                .unwrap_err()
                .contains("不存在")
        );
    }
}
