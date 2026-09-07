//! Artifact import — the `finetune.import` RPC.
//!
//! The last leg of "curate here, train elsewhere, **deploy here**": take a
//! GGUF or LoRA adapter that a remote run produced and put it where this box
//! already looks for models.
//!
//! That place is `<DUDUCLAW_HOME>/models` — the same directory
//! `local_models::installed` scans and `duduclaw_inference`'s
//! `InferenceConfig::models_dir` resolves to. Importing writes one file
//! there and stops; there is no second registry to keep in sync, because in
//! this design the file **is** the registry. A `.gguf` dropped here shows up
//! in the local-models list on the next refresh with no further plumbing.
//!
//! Accepted inputs:
//!
//! - an absolute local path (typically `<job dir>/artifacts/<name>`, which is
//!   where the SSH backend puts what it fetched off the GPU host);
//! - an `https://` URL (`http://` is refused — a model file is executable
//!   input to an inference engine and is not fetched over plaintext).
//!
//! Accepted extensions: `.gguf` (quantised weights or a converted LoRA) and
//! `.safetensors` / `.bin` (a raw PEFT adapter). Anything else is refused by
//! name rather than copied and left to fail later inside llama.cpp.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{io_err, models_dir, FinetuneError, Result};

/// Extensions we will place in the models directory.
const ALLOWED_EXTENSIONS: &[&str] = &["gguf", "safetensors", "bin"];

/// Refuse anything that is not plausibly a model artifact, so a mistyped
/// path cannot fill the models directory with junk the inference engine will
/// choke on later.
pub fn validate_artifact_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name.len() > 200
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.');
    if bad {
        return Err(FinetuneError::BadRequest(format!("檔名不合法：{name}")));
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !ALLOWED_EXTENSIONS.contains(&ext.as_str()) {
        return Err(FinetuneError::BadRequest(format!(
            "只接受 .gguf、.safetensors、.bin：{name}"
        )));
    }
    Ok(())
}

/// Pull the destination filename out of a path or URL.
pub fn artifact_filename(path_or_url: &str) -> Result<String> {
    let trimmed = path_or_url.trim();
    if trimmed.is_empty() {
        return Err(FinetuneError::BadRequest("path_or_url 不可空白".into()));
    }
    let tail = if is_url(trimmed) {
        // Strip query/fragment before taking the basename — a HF `resolve`
        // URL commonly carries `?download=true`.
        trimmed
            .split(['?', '#'])
            .next()
            .unwrap_or(trimmed)
            .rsplit('/')
            .next()
            .unwrap_or_default()
    } else {
        Path::new(trimmed)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default()
    };
    let name = tail.to_string();
    validate_artifact_name(&name)?;
    Ok(name)
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// `finetune.import` — copy or download one artifact into
/// `<home>/models` and report what landed.
///
/// Not privacy-gated: this direction moves data *onto* the machine.
pub async fn import(home: &Path, path_or_url: &str) -> Result<Value> {
    let src = path_or_url.trim();
    let filename = artifact_filename(src)?;
    let dest_dir = models_dir(home);
    std::fs::create_dir_all(&dest_dir).map_err(|e| io_err(&dest_dir, e))?;
    let dest = dest_dir.join(&filename);

    if is_url(src) {
        if src.starts_with("http://") {
            return Err(FinetuneError::BadRequest(
                "模型檔只接受 https://，不走明文 http".into(),
            ));
        }
        // Reuse the marketplace downloader: resumable, and it re-validates
        // the filename against traversal on its own.
        duduclaw_inference::model_registry::downloader::download_model(
            src, "", &dest_dir, &filename, None,
        )
        .await
        .map_err(|e| FinetuneError::Backend(format!("下載失敗：{e}")))?;
    } else {
        let source = PathBuf::from(src);
        if !source.is_file() {
            return Err(FinetuneError::NotFound(format!("檔案 {src}")));
        }
        if source == dest {
            return Err(FinetuneError::BadRequest(
                "來源就是模型目錄裡的同一個檔案，不需要匯入".into(),
            ));
        }
        std::fs::copy(&source, &dest).map_err(|e| io_err(&dest, e))?;
    }

    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "filename": filename,
        "path": dest.display().to_string(),
        "size_bytes": size,
        "models_dir": dest_dir.display().to_string(),
        "note": "已放進本地模型目錄，重新整理「本地模型」頁就會看到。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_model_artifact_extensions_are_accepted() {
        assert!(validate_artifact_name("qwen3-4b-finance-lora.gguf").is_ok());
        assert!(validate_artifact_name("adapter_model.safetensors").is_ok());
        assert!(validate_artifact_name("adapter_model.bin").is_ok());
        for bad in [
            "notes.txt",
            "train.yaml",
            "model",
            "",
            "../escape.gguf",
            "a/b.gguf",
            "a\\b.gguf",
            ".hidden.gguf",
        ] {
            assert!(validate_artifact_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn filename_comes_from_the_path_or_url_tail_minus_query() {
        assert_eq!(
            artifact_filename("/srv/jobs/j1/artifacts/adapter_model.safetensors").unwrap(),
            "adapter_model.safetensors"
        );
        assert_eq!(
            artifact_filename("https://huggingface.co/org/repo/resolve/main/m.gguf?download=true")
                .unwrap(),
            "m.gguf"
        );
        assert_eq!(artifact_filename("  https://x/y/z.gguf  ").unwrap(), "z.gguf");
        assert!(artifact_filename("").is_err());
        assert!(artifact_filename("https://example.com/evil.sh").is_err());
        // Traversal in a URL path still ends at a rejected basename.
        assert!(artifact_filename("https://x/../../etc/passwd").is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_import_lands_in_the_same_models_dir_local_models_scans() {
        let home = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("finance-lora.gguf");
        std::fs::write(&src, b"GGUF-fake-bytes").unwrap();

        let out = import(home.path(), src.to_str().unwrap()).await.unwrap();
        assert_eq!(out["filename"], "finance-lora.gguf");
        assert_eq!(out["size_bytes"], 15);
        let landed = home.path().join("models").join("finance-lora.gguf");
        assert!(landed.exists());
        assert_eq!(out["path"], landed.display().to_string());

        // And `local_models::installed` — the page this feeds — sees it.
        let listed = crate::local_models::installed(home.path()).await;
        let names: Vec<&str> = listed["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["filename"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"finance-lora.gguf"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn import_refuses_missing_files_plaintext_http_and_wrong_types() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            import(home.path(), "/no/such/file.gguf").await.unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            import(home.path(), "http://example.com/m.gguf").await.unwrap_err().code(),
            "bad_request"
        );
        assert_eq!(
            import(home.path(), "/tmp/README.md").await.unwrap_err().code(),
            "bad_request"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn importing_a_file_already_in_the_models_dir_is_refused_not_truncated() {
        let home = tempfile::tempdir().unwrap();
        let models = home.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let f = models.join("already.gguf");
        std::fs::write(&f, b"bytes").unwrap();
        assert_eq!(
            import(home.path(), f.to_str().unwrap()).await.unwrap_err().code(),
            "bad_request"
        );
        // The original is intact — a self-copy must never zero the file.
        assert_eq!(std::fs::read(&f).unwrap(), b"bytes");
    }
}
