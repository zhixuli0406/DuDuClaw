//! Media processing pipeline — image resize, MIME detection, base64 encoding.
//!
//! Handles attachments from channel messages (Telegram photos, Discord attachments,
//! LINE image messages) and prepares them for Claude Vision API.

use base64::Engine;
use tracing::warn;

/// Maximum image dimension (Claude Vision recommendation).
const MAX_IMAGE_DIM: u32 = 1568;

/// Maximum file size in bytes (20MB).
pub const MAX_FILE_SIZE: u64 = 20 * 1024 * 1024;

/// Supported media types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaType {
    Image,
    Audio,
    Video,
    File,
}

/// An attachment from a channel message.
#[derive(Debug, Clone)]
pub struct MediaAttachment {
    pub media_type: MediaType,
    pub data: Vec<u8>,
    pub mime: String,
    pub filename: Option<String>,
    pub size_bytes: u64,
}

/// Detect MIME type from magic bytes.
pub fn detect_mime(data: &[u8]) -> String {
    if data.len() < 4 {
        return "application/octet-stream".to_string();
    }

    match &data[..4] {
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg".to_string(),
        [0x89, 0x50, 0x4E, 0x47] => "image/png".to_string(),
        [0x47, 0x49, 0x46, 0x38] => "image/gif".to_string(),
        [0x52, 0x49, 0x46, 0x46] => {
            // Could be WebP or WAV
            if data.len() >= 12 && &data[8..12] == b"WEBP" {
                "image/webp".to_string()
            } else if data.len() >= 12 && &data[8..12] == b"WAVE" {
                "audio/wav".to_string()
            } else {
                "application/octet-stream".to_string()
            }
        }
        // OGG (Opus voice messages from Telegram)
        [0x4F, 0x67, 0x67, 0x53] => "audio/ogg".to_string(),
        // MP3 (ID3 tag)
        [0x49, 0x44, 0x33, ..] => "audio/mpeg".to_string(),
        // MP3 (sync word)
        [0xFF, 0xFB, ..] | [0xFF, 0xF3, ..] | [0xFF, 0xF2, ..] => "audio/mpeg".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Resize an image to fit within MAX_IMAGE_DIM, maintaining aspect ratio.
/// Returns JPEG bytes at 85% quality.
pub fn resize_image(data: &[u8], max_dim: u32) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(data)
        .map_err(|e| format!("Failed to decode image: {e}"))?;

    let (w, h) = (img.width(), img.height());
    let max_side = w.max(h);

    let resized = if max_side > max_dim {
        img.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    // Encode as JPEG at 85% quality
    let mut buf = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut buf, image::ImageFormat::Jpeg)
        .map_err(|e| format!("Failed to encode image: {e}"))?;

    Ok(buf.into_inner())
}

/// Convert image data to base64 data URI for Claude Vision API.
pub fn to_base64_data_uri(data: &[u8], mime: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    format!("data:{mime};base64,{b64}")
}

/// Process an image attachment for Claude Vision: resize + encode.
pub fn prepare_image_for_vision(attachment: &MediaAttachment) -> Result<(String, String), String> {
    if attachment.size_bytes > MAX_FILE_SIZE {
        return Err(format!(
            "Image too large: {} bytes (max {})",
            attachment.size_bytes, MAX_FILE_SIZE
        ));
    }

    // Always detect MIME from content, don't trust external claim
    let mime = detect_mime(&attachment.data);

    // Resize if needed
    let processed = match resize_image(&attachment.data, MAX_IMAGE_DIM) {
        Ok(resized) => resized,
        Err(e) => {
            warn!("Image resize failed, using original: {e}");
            attachment.data.clone()
        }
    };

    let data_uri = to_base64_data_uri(&processed, "image/jpeg");
    Ok((data_uri, mime))
}

/// Build a Claude Vision API content block from an image.
pub fn vision_content_block(base64_data: &str, media_type: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": base64_data.strip_prefix(&format!("data:{media_type};base64,")).unwrap_or(base64_data),
        }
    })
}

// ── Attachment persistence ──────────────────────────────────────

/// Save attachment bytes to `{home_dir}/attachments/{unique_name}` and return the
/// absolute path. Creates the directory if it does not exist.
///
/// The saved path can be included in the message text so that Claude Code's
/// `Read` tool can access the file directly.
pub async fn save_attachment_to_disk(
    home_dir: &std::path::Path,
    data: &[u8],
    filename: &str,
) -> Result<std::path::PathBuf, String> {
    let dir = home_dir.join("attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create attachments dir: {e}"))?;

    // Sanitize filename: keep only safe characters
    let safe_name: String = filename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let unique_name = format!("{ts}_{safe_name}");
    let path = dir.join(&unique_name);

    tokio::fs::write(&path, data)
        .await
        .map_err(|e| format!("Failed to write attachment: {e}"))?;

    Ok(path)
}

/// Format an attachment reference for inclusion in message text sent to the agent.
///
/// Returns a markdown-style line like `[📎 photo.jpg (image)](file:///path/to/file)`.
pub fn format_attachment_ref(media_type: &MediaType, filename: &str, path: &std::path::Path) -> String {
    let emoji = match media_type {
        MediaType::Image => "🖼️",
        MediaType::Audio => "🎵",
        MediaType::Video => "🎬",
        MediaType::File => "📎",
    };
    let type_label = match media_type {
        MediaType::Image => "image",
        MediaType::Audio => "audio",
        MediaType::Video => "video",
        MediaType::File => "file",
    };
    format!("[{emoji} {filename} ({type_label})]({})", path.display())
}

/// Download a file from a URL with an optional auth header.
pub async fn download_url(
    http: &reqwest::Client,
    url: &str,
    auth_header: Option<(&str, &str)>,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut req = http.get(url);
    if let Some((key, value)) = auth_header {
        req = req.header(key, value);
    }
    let resp = req.send().await.map_err(|e| format!("Download failed: {e}"))?;

    if let Some(len) = resp.content_length() {
        if len > max_bytes as u64 {
            return Err(format!("File too large: {len} bytes (max {max_bytes})"));
        }
    }

    let bytes = resp.bytes().await.map_err(|e| format!("Download bytes: {e}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("File too large: {} bytes (max {max_bytes})", bytes.len()));
    }
    Ok(bytes.to_vec())
}

/// Infer media type from MIME string.
pub fn media_type_from_mime(mime: &str) -> MediaType {
    if mime.starts_with("image/") {
        MediaType::Image
    } else if mime.starts_with("audio/") {
        MediaType::Audio
    } else if mime.starts_with("video/") {
        MediaType::Video
    } else {
        MediaType::File
    }
}

/// Infer a file extension from MIME type.
pub fn extension_from_mime(mime: &str) -> &str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/aac" => "aac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mime_jpeg() {
        assert_eq!(detect_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
    }

    #[test]
    fn test_detect_mime_png() {
        assert_eq!(detect_mime(&[0x89, 0x50, 0x4E, 0x47]), "image/png");
    }

    #[test]
    fn test_detect_mime_gif() {
        assert_eq!(detect_mime(&[0x47, 0x49, 0x46, 0x38]), "image/gif");
    }

    #[test]
    fn test_detect_mime_ogg() {
        assert_eq!(detect_mime(&[0x4F, 0x67, 0x67, 0x53]), "audio/ogg");
    }

    #[test]
    fn test_detect_mime_unknown() {
        assert_eq!(detect_mime(&[0x00, 0x00, 0x00, 0x00]), "application/octet-stream");
    }

    #[test]
    fn test_detect_mime_short() {
        assert_eq!(detect_mime(&[0xFF]), "application/octet-stream");
    }

    #[test]
    fn test_to_base64_data_uri() {
        let data = b"hello";
        let uri = to_base64_data_uri(data, "text/plain");
        assert!(uri.starts_with("data:text/plain;base64,"));
    }
}
