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
    // Pre-load verification errors remain retryable. Once ort is called, any
    // load/identity failure poisons INIT permanently, before a session can exist.
    static VERIFIED: OnceLock<crate::install::verified_file::VerifiedLibrary> = OnceLock::new();
    let verified = if sealed { None } else {
        let pins = crate::manifest::ort_lib_pins(crate::manifest::current_target())
            .ok_or_else(|| "這個平台沒有可用的 ONNX Runtime".to_string())?;
        Some(crate::install::verified_file::verify_for_load(lib_path, pins)?)
    };
    #[cfg(target_os = "linux")]
    let load_path = if sealed { sealed_runtime::prepare(lib_path)? } else { lib_path.to_path_buf() };
    #[cfg(not(target_os = "linux"))]
    let load_path = {
        if sealed {
            return Err("這個平台不支援獨立金鑰模式的 ONNX Runtime 安全載入".into());
        }
        lib_path.to_path_buf()
    };
    if let Some(verified) = verified {
        VERIFIED.set(verified).map_err(|_| "ONNX Runtime 初始化狀態衝突".to_string())?;
    }
    let _ = INIT.get_or_init(|| {
        let result: Result<_, String> = (|| {
            // SECURITY: Path-based loading cannot prevent a same-user process
            // from replacing the file between verification and the dynamic loader
            // opening it. ctime + post-load re-hash detect it and block all sessions,
            // but native initialisers of a replaced library would already have run.
            // The mitigation boundary is that employee processes must not have write
            // access to <home>/lib (enforced by the owner/mode rule and employee
            // sandbox); protected Linux installs load from a sealed memfd.
            let loaded = ort::init_from(&load_path);
            // Check immediately on return, including failed loads. The handle
            // remains retained even when this check or environment creation fails.
            if let Some(verified) = VERIFIED.get() {
                verified.check_after_load(lib_path)?;
            }
            let builder = loaded
                .map_err(|e| format!("載入 ONNX Runtime 失敗（{}）：{e}", lib_path.display()))?;
            #[cfg(target_os = "linux")]
            if sealed {
                sealed_runtime::verify_loaded()?;
            }
            let env = builder.build().map_err(|e| format!("建立 ONNX Runtime 環境失敗：{e}"))?;
            Ok((env, sealed))
        })();
        if let Err(error) = &result {
            eprintln!("ONNX Runtime 初始化失敗：{}", error.replace(['\n', '\r'], " "));
        }
        result
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
