//! Model downloader — async HTTP download with progress, resume, and mirror support.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::error::{InferenceError, Result};

/// Download progress callback.
pub type ProgressCallback = Box<dyn Fn(DownloadProgress) + Send>;

/// Download progress information.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bytes_per_sec: f64,
    pub eta_seconds: f64,
}

impl DownloadProgress {
    pub fn percent(&self) -> f64 {
        if self.total_bytes > 0 {
            (self.downloaded_bytes as f64 / self.total_bytes as f64) * 100.0
        } else {
            0.0
        }
    }

    pub fn display_speed(&self) -> String {
        let mb = self.speed_bytes_per_sec / (1024.0 * 1024.0);
        format!("{:.1} MB/s", mb)
    }

    pub fn display_eta(&self) -> String {
        let secs = self.eta_seconds as u64;
        if secs > 3600 {
            format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
        } else if secs > 60 {
            format!("{}m{}s", secs / 60, secs % 60)
        } else {
            format!("{}s", secs)
        }
    }
}

/// Download a model file from URL to the models directory.
///
/// Supports:
/// - Resume via HTTP Range headers (.partial temp file)
/// - Automatic mirror fallback (hf-mirror.com)
/// - Progress callbacks for UI
pub async fn download_model(
    url: &str,
    mirror_url: &str,
    dest_dir: &Path,
    filename: &str,
    on_progress: Option<ProgressCallback>,
) -> Result<PathBuf> {
    let dest = dest_dir.join(filename);
    if dest.exists() {
        info!(path = %dest.display(), "Model already exists, skipping download");
        return Ok(dest);
    }

    tokio::fs::create_dir_all(dest_dir).await?;

    // Try primary URL first, then mirror
    let result = download_with_resume(url, dest_dir, filename, &on_progress).await;
    match result {
        Ok(path) => Ok(path),
        Err(e) => {
            if mirror_url.is_empty() || mirror_url == url {
                return Err(e);
            }
            warn!(error = %e, "Primary download failed, trying mirror");
            download_with_resume(mirror_url, dest_dir, filename, &on_progress).await
        }
    }
}

/// Download with resume support.
async fn download_with_resume(
    url: &str,
    dest_dir: &Path,
    filename: &str,
    on_progress: &Option<ProgressCallback>,
) -> Result<PathBuf> {
    let dest = dest_dir.join(filename);
    let partial = dest_dir.join(format!("{filename}.partial"));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600)) // 1 hour max
        .build()
        .map_err(|e| InferenceError::Http(format!("Failed to create HTTP client: {e}")))?;

    // Check existing partial download
    let existing_size = if partial.exists() {
        tokio::fs::metadata(&partial).await.map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Build request with Range header for resume
    let mut req = client.get(url);
    let hf_token = std::env::var("HF_TOKEN").unwrap_or_default();
    if !hf_token.is_empty() {
        req = req.bearer_auth(&hf_token);
    }
    if existing_size > 0 {
        req = req.header("Range", format!("bytes={existing_size}-"));
        info!(resume_from = existing_size, "Resuming download");
    }

    let resp = req.send().await
        .map_err(|e| InferenceError::Http(format!("Download request failed: {e}")))?;

    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(InferenceError::Http(format!(
            "Download HTTP {}: {}", resp.status(), url
        )));
    }

    // Get total size from Content-Length or Content-Range
    let total_bytes = if resp.status().as_u16() == 206 {
        // Partial content — parse Content-Range: bytes 1000-9999/10000
        resp.headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split('/').last())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    } else {
        resp.content_length().unwrap_or(0)
    };

    // Open file for writing (append if resuming)
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(existing_size > 0)
        .write(true)
        .truncate(existing_size == 0)
        .open(&partial)
        .await?;

    // Stream download with progress
    let mut downloaded = existing_size;
    let start = std::time::Instant::now();
    let mut stream = resp.bytes_stream();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| InferenceError::Http(format!("Download stream error: {e}")))?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        if let Some(cb) = on_progress {
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                (downloaded - existing_size) as f64 / elapsed
            } else {
                0.0
            };
            let remaining = if speed > 0.0 && total_bytes > downloaded {
                (total_bytes - downloaded) as f64 / speed
            } else {
                0.0
            };

            cb(DownloadProgress {
                downloaded_bytes: downloaded,
                total_bytes,
                speed_bytes_per_sec: speed,
                eta_seconds: remaining,
            });
        }
    }

    file.flush().await?;
    drop(file);

    // Rename partial to final
    tokio::fs::rename(&partial, &dest).await?;

    info!(
        path = %dest.display(),
        size_mb = downloaded / (1024 * 1024),
        "Model download complete"
    );

    Ok(dest)
}

/// Check if HuggingFace CDN is reachable (5s timeout).
pub async fn is_hf_reachable() -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    matches!(
        client.head("https://huggingface.co").send().await,
        Ok(r) if r.status().is_success() || r.status().is_redirection()
    )
}
