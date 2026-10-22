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
    if INIT.get().is_none() && !lib_path.exists() {
        return Err(format!("ONNX Runtime 執行庫不存在：{}", lib_path.display()));
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
