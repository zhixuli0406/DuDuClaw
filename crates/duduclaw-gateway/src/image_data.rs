//! Bounded raster data-URI validation for employee avatars.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

const IMAGE_PREFIXES: &[(&str, ImageKind)] = &[
    ("data:image/png;base64,", ImageKind::Png),
    ("data:image/jpeg;base64,", ImageKind::Jpeg),
    ("data:image/webp;base64,", ImageKind::Webp),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageKind {
    Png,
    Jpeg,
    Webp,
}

impl ImageKind {
    /// File-extension token used for on-disk avatar files + mime lookup.
    pub(crate) fn ext(self) -> &'static str {
        match self {
            ImageKind::Png => "png",
            ImageKind::Jpeg => "jpg",
            ImageKind::Webp => "webp",
        }
    }
}

/// A raster image validated + decoded from a base64 data URI.
pub(crate) struct ValidatedImage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) kind: ImageKind,
}

pub(crate) fn validate_image_data_uri(
    uri: &str,
    max_decoded: usize,
    label: &str,
) -> Result<ValidatedImage, String> {
    let (b64, kind) = IMAGE_PREFIXES
        .iter()
        .find_map(|(prefix, kind)| uri.strip_prefix(prefix).map(|rest| (rest, *kind)))
        .ok_or_else(|| {
            format!("{label} 格式不支援：僅接受 PNG / JPEG / WebP 的 base64 data URI（不接受 SVG）")
        })?;
    let b64 = b64.trim();

    // Bound the *encoded* length before allocating the decode buffer (base64
    // inflates ~4/3; +4 for padding slack) so an oversized payload is rejected
    // up front rather than after a large decode allocation.
    let max_b64_len = max_decoded / 3 * 4 + 4;
    if b64.len() > max_b64_len {
        return Err(format!(
            "{label} 檔案過大（上限 {} KB）",
            max_decoded / 1024
        ));
    }

    let bytes = BASE64
        .decode(b64)
        .map_err(|_| format!("{label} base64 解碼失敗"))?;
    if bytes.len() > max_decoded {
        return Err(format!(
            "{label} 檔案過大（上限 {} KB）",
            max_decoded / 1024
        ));
    }
    if !magic_matches(&bytes, kind) {
        return Err(format!(
            "{label} 內容與宣告的格式不符（magic bytes 驗證失敗）"
        ));
    }
    Ok(ValidatedImage { bytes, kind })
}

/// Confirm the decoded bytes carry the signature of the declared image kind.
/// This defeats a payload that declares `image/png` but ships an SVG/HTML body.
fn magic_matches(bytes: &[u8], kind: ImageKind) -> bool {
    match kind {
        ImageKind::Png => {
            bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        }
        ImageKind::Jpeg => bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF],
        ImageKind::Webp => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
    }
}
