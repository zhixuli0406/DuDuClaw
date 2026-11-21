//! ONNX Runtime bootstrap (`load-dynamic`).

use std::path::Path;
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
#[path = "sealed_runtime.rs"]
mod sealed_runtime;

static INIT: OnceLock<Result<(ort::environment::Environment, bool), String>> = OnceLock::new();
static INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Load `libonnxruntime` from the installed path, once per process.
///
/// With ort's `load-dynamic` feature the binary carries no ONNX Runtime; the
/// library is whatever the install step put under
/// `<home>/lib/onnxruntime/<ver>/`. The environment and any load/build failure
/// are memoised — and shared by every
/// feature in the process (NER redaction, knowledge embeddings). The first
/// caller's path wins; every caller passes the same versioned path.
///
/// A library that is not there yet is reported without being memoised, so
/// installing it later in the same process still works.
pub fn init_ort(lib_path: &Path) -> Result<(), String> {
    let _lock = INIT_LOCK.lock().map_err(|_| "ONNX Runtime 初始化鎖無法使用".to_string())?;
    if INIT.get().is_some() {
        return environment().map(|_| ());
    }
    if !lib_path.exists() {
        return Err(format!("ONNX Runtime 執行庫不存在：{}", lib_path.display()));
    }
    let sealed = duduclaw_core::root_key::is_held();
    // Verification errors are retryable after reinstall. Only a verified path
    // reaches ort; memoise the actual load/build outcome once per process.
    #[cfg(target_os = "linux")]
    let load_path = if sealed {
        sealed_runtime::prepare(lib_path)?
    } else {
        crate::install::verify_ort_lib_path(lib_path)?;
        lib_path.to_path_buf()
    };
    #[cfg(not(target_os = "linux"))]
    let load_path = {
        if sealed {
            return Err("這個平台不支援獨立金鑰模式的 ONNX Runtime 安全載入".into());
        }
        crate::install::verify_ort_lib_path(lib_path)?;
        lib_path.to_path_buf()
    };
    let _ = INIT.get_or_init(|| {
        let builder = ort::init_from(&load_path)
            .map_err(|e| format!("載入 ONNX Runtime 失敗（{}）：{e}", lib_path.display()))?;
        #[cfg(target_os = "linux")]
        if sealed {
            sealed_runtime::verify_loaded(&load_path)?;
        }
        let env = builder.build().map_err(|e| format!("建立 ONNX Runtime 環境失敗：{e}"))?;
        Ok((env, sealed))
    });
    environment().map(|_| ())
}

/// The process environment after successful [`init_ort`]. Never implicitly
/// loads a library or creates an environment. An unsealed load cannot later
/// be reused while the process holds a protected root key.
pub fn environment() -> Result<&'static ort::environment::Environment, String> {
    let (env, sealed) = INIT.get()
        .ok_or_else(|| "ONNX Runtime 尚未經過驗證與初始化".to_string())?
        .as_ref().map_err(Clone::clone)?;
    if duduclaw_core::root_key::is_held() && !sealed {
        return Err("ONNX Runtime 已由未封存路徑初始化，拒絕在獨立金鑰模式使用".into());
    }
    Ok(env)
}

/// Create a session builder bound to the verified, shared process environment.
pub fn session_builder() -> Result<ort::session::builder::SessionBuilder, String> {
    ort::session::Session::builder(environment()?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_builder_refuses_an_uninitialized_environment() {
        assert!(environment().is_err());
        assert!(session_builder().is_err());
    }

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
