//! Background removal — turn a photo into an RGBA cutout PNG.
//!
//! Two paths:
//! - [`PassthroughRemover`] (always available): decodes any input and re-encodes
//!   it as RGBA PNG **without** removing anything. It backs the "external cutout"
//!   flow (user already removed the background) and is the guaranteed fallback
//!   when no model is present.
//! - [`OnnxRemover`] (feature `onnx`): local ONNX inference with
//!   BiRefNet-general-lite (primary) or silueta (low-resource fallback), matting
//!   the subject and writing its mask into the alpha channel.
//!
//! Model files live in `~/.duduclaw/models/` (shared with the GGUF/embedding
//! model store). They are **not** downloaded at build time — see [`ModelSpec`]
//! and `download_model`.

use crate::error::{PetError, Result};

/// A background remover: encoded image bytes in, RGBA PNG bytes out.
pub trait BackgroundRemover {
    /// Remove (or, for passthrough, retain) the background and return RGBA PNG bytes.
    fn remove_background(&self, image_bytes: &[u8]) -> Result<Vec<u8>>;
    /// Human-facing label for logs / UI ("passthrough", "birefnet", "silueta").
    fn label(&self) -> &'static str;
}

// ── Passthrough ──────────────────────────────────────────────────────────────

/// Re-encodes the input as RGBA PNG unchanged. Used for the external-cutout path
/// and as the always-available fallback.
pub struct PassthroughRemover;

impl BackgroundRemover for PassthroughRemover {
    fn remove_background(&self, image_bytes: &[u8]) -> Result<Vec<u8>> {
        let img = image::load_from_memory(image_bytes)
            .map_err(|e| PetError::Image(format!("decode failed: {e}")))?;
        encode_png_rgba(&img.to_rgba8())
    }
    fn label(&self) -> &'static str {
        "passthrough"
    }
}

/// Encode an RGBA image buffer as PNG bytes.
pub(crate) fn encode_png_rgba(rgba: &image::RgbaImage) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba.clone())
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| PetError::Image(format!("PNG encode failed: {e}")))?;
    Ok(out.into_inner())
}

// ── Model registry ───────────────────────────────────────────────────────────

/// Which segmentation model to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelVariant {
    /// BiRefNet-general-lite — DIS-family SOTA, ~213MB, MIT. Primary.
    Birefnet,
    /// silueta — u2net-family, ~43MB, Apache-2.0. Low-resource fallback.
    Silueta,
}

/// Static description of a downloadable model: local filename, source URL, the
/// expected SHA-256 (once pinned), and the square input side the net expects.
pub struct ModelSpec {
    pub variant: ModelVariant,
    /// Filename under `~/.duduclaw/models/`.
    pub filename: &'static str,
    /// HTTPS source URL.
    ///
    /// NOTE: these point at the upstream community artifacts but the exact
    /// revision path should be confirmed against the live repos before shipping
    /// a pinned checksum (see `expected_sha256`). Downloading logs the computed
    /// digest so the operator can pin it.
    pub url: &'static str,
    /// Expected lowercase hex SHA-256. `None` = not yet pinned: the download is
    /// accepted and its computed digest is logged (never fabricate a checksum).
    pub expected_sha256: Option<&'static str>,
    /// Square input side (px) the network expects.
    pub input_size: u32,
}

/// BiRefNet-general-lite spec. URL/checksum to be confirmed against upstream.
pub const BIREFNET_LITE: ModelSpec = ModelSpec {
    variant: ModelVariant::Birefnet,
    filename: "birefnet-general-lite.onnx",
    url: "https://huggingface.co/onnx-community/BiRefNet_lite/resolve/main/onnx/model.onnx",
    expected_sha256: None,
    input_size: 1024,
};

/// silueta spec (rembg release artifact). URL/checksum to be confirmed upstream.
pub const SILUETA: ModelSpec = ModelSpec {
    variant: ModelVariant::Silueta,
    filename: "silueta.onnx",
    url: "https://github.com/danielgatis/rembg/releases/download/v0.0.0/silueta.onnx",
    expected_sha256: None,
    input_size: 320,
};

impl ModelSpec {
    /// Resolve the on-disk path (`~/.duduclaw/models/<filename>`).
    pub fn local_path(&self) -> std::path::PathBuf {
        models_dir().join(self.filename)
    }
    /// Whether the model file already exists locally.
    pub fn is_present(&self) -> bool {
        self.local_path().is_file()
    }
}

/// The shared model directory (`~/.duduclaw/models/`).
pub fn models_dir() -> std::path::PathBuf {
    duduclaw_core::duduclaw_home().join("models")
}

/// Compute the lowercase hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ── ONNX remover (feature-gated) ─────────────────────────────────────────────

#[cfg(feature = "onnx")]
mod onnx_impl {
    use super::*;
    use ort::session::{builder::GraphOptimizationLevel, Session};

    /// Local ONNX-based background remover.
    pub struct OnnxRemover {
        // ort 2.0.0-rc.12's `Session::run` takes `&mut self`; the
        // `BackgroundRemover` trait is `&self`, so guard with a Mutex.
        session: std::sync::Mutex<Session>,
        input_size: u32,
        variant: ModelVariant,
    }

    impl OnnxRemover {
        /// Load a model from `~/.duduclaw/models/`. Errors (with the download URL)
        /// when the file is missing so the caller can prompt/download.
        pub fn load(spec: &ModelSpec) -> Result<Self> {
            let path = spec.local_path();
            if !path.is_file() {
                return Err(PetError::ModelUnavailable(format!(
                    "{} not found at {} — download from {}",
                    spec.filename,
                    path.display(),
                    spec.url
                )));
            }
            let threads = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(2);
            let session = Session::builder()
                .map_err(|e| PetError::Inference(format!("session builder: {e}")))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| PetError::Inference(format!("optimization: {e}")))?
                .with_intra_threads(threads)
                .map_err(|e| PetError::Inference(format!("threads: {e}")))?
                .commit_from_file(&path)
                .map_err(|e| PetError::Inference(format!("load {}: {e}", path.display())))?;
            Ok(Self {
                session: std::sync::Mutex::new(session),
                input_size: spec.input_size,
                variant: spec.variant,
            })
        }

        /// ImageNet normalization used by both BiRefNet and (rembg) u2net/silueta.
        fn normalize(&self, r: u8, g: u8, b: u8) -> [f32; 3] {
            const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
            const STD: [f32; 3] = [0.229, 0.224, 0.225];
            [
                (r as f32 / 255.0 - MEAN[0]) / STD[0],
                (g as f32 / 255.0 - MEAN[1]) / STD[1],
                (b as f32 / 255.0 - MEAN[2]) / STD[2],
            ]
        }
    }

    impl BackgroundRemover for OnnxRemover {
        fn remove_background(&self, image_bytes: &[u8]) -> Result<Vec<u8>> {
            use image::imageops::FilterType;
            use ndarray::Array4;

            let orig = image::load_from_memory(image_bytes)
                .map_err(|e| PetError::Image(format!("decode failed: {e}")))?
                .to_rgb8();
            let (ow, oh) = (orig.width(), orig.height());
            let side = self.input_size;

            // Resize to the square model input.
            let resized = image::imageops::resize(&orig, side, side, FilterType::Triangle);

            // Build an NCHW f32 tensor with ImageNet normalization.
            let mut data = vec![0.0f32; (3 * side * side) as usize];
            let plane = (side * side) as usize;
            for y in 0..side {
                for x in 0..side {
                    let px = resized.get_pixel(x, y);
                    let n = self.normalize(px[0], px[1], px[2]);
                    let idx = (y * side + x) as usize;
                    data[idx] = n[0];
                    data[plane + idx] = n[1];
                    data[2 * plane + idx] = n[2];
                }
            }
            let input = Array4::from_shape_vec((1, 3, side as usize, side as usize), data)
                .map_err(|e| PetError::Inference(format!("tensor shape: {e}")))?;

            // Run inference.
            let input_tensor = ort::value::Tensor::from_array(input)
                .map_err(|e| PetError::Inference(format!("inputs: {e}")))?;
            let mut session = self
                .session
                .lock()
                .map_err(|_| PetError::Inference("session lock poisoned".into()))?;
            let outputs = session
                .run(ort::inputs![input_tensor])
                .map_err(|e| PetError::Inference(format!("run: {e}")))?;

            // The BiRefNet-lite / silueta ONNX exports emit a single output: the
            // foreground map.
            let out = outputs
                .values()
                .next()
                .ok_or_else(|| PetError::Inference("no output tensor".into()))?;
            let (out_shape, out_data) = out
                .try_extract_tensor::<f32>()
                .map_err(|e| PetError::Inference(format!("extract: {e}")))?;
            let shape: Vec<usize> = out_shape.iter().map(|d| *d as usize).collect();
            let flat: Vec<f32> = out_data.to_vec();

            // The trailing two dims are the mask HxW.
            let (mh, mw) = match shape.len() {
                n if n >= 2 => (shape[n - 2], shape[n - 1]),
                _ => return Err(PetError::Inference(format!("bad mask shape {shape:?}"))),
            };
            let want = mh * mw;
            if flat.len() < want {
                return Err(PetError::Inference(format!(
                    "mask data {} < {mh}x{mw}",
                    flat.len()
                )));
            }
            // Take the last mh*mw values (final map when multiple are concatenated).
            let mask = &flat[flat.len() - want..];

            // Min-max normalize to [0,1] (robust across sigmoid/logit outputs).
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for &v in mask {
                lo = lo.min(v);
                hi = hi.max(v);
            }
            let range = (hi - lo).max(1e-6);

            // Rasterize the mask to a grayscale image, then resize to the original.
            let mut mask_img = image::GrayImage::new(mw as u32, mh as u32);
            for (i, &v) in mask.iter().enumerate() {
                let a = (((v - lo) / range) * 255.0).clamp(0.0, 255.0) as u8;
                let (x, y) = ((i % mw) as u32, (i / mw) as u32);
                mask_img.put_pixel(x, y, image::Luma([a]));
            }
            let mask_full =
                image::imageops::resize(&mask_img, ow, oh, image::imageops::FilterType::Triangle);

            // Compose: original RGB + mask alpha.
            let mut rgba = image::RgbaImage::new(ow, oh);
            for y in 0..oh {
                for x in 0..ow {
                    let p = orig.get_pixel(x, y);
                    let a = mask_full.get_pixel(x, y)[0];
                    rgba.put_pixel(x, y, image::Rgba([p[0], p[1], p[2], a]));
                }
            }
            let _ = self.variant; // reserved for per-variant post-processing tuning
            encode_png_rgba(&rgba)
        }

        fn label(&self) -> &'static str {
            match self.variant {
                ModelVariant::Birefnet => "birefnet",
                ModelVariant::Silueta => "silueta",
            }
        }
    }

    /// Download a model to `~/.duduclaw/models/` and verify its checksum.
    ///
    /// If `spec.expected_sha256` is pinned, a mismatch is a hard error. Otherwise
    /// the computed digest is returned so the operator can pin it. Never downloads
    /// at build time — this is an explicit runtime action.
    pub fn download_model(spec: &ModelSpec) -> Result<String> {
        std::fs::create_dir_all(models_dir())?;
        let resp = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| PetError::ModelDownload(e.to_string()))?
            .get(spec.url)
            .send()
            .map_err(|e| PetError::ModelDownload(format!("GET {}: {e}", spec.url)))?;
        if !resp.status().is_success() {
            return Err(PetError::ModelDownload(format!(
                "HTTP {} for {}",
                resp.status(),
                spec.url
            )));
        }
        let bytes = resp
            .bytes()
            .map_err(|e| PetError::ModelDownload(e.to_string()))?;
        let digest = sha256_hex(&bytes);
        if let Some(expected) = spec.expected_sha256 {
            if !digest.eq_ignore_ascii_case(expected) {
                return Err(PetError::ModelDownload(format!(
                    "checksum mismatch for {}: expected {expected}, got {digest}",
                    spec.filename
                )));
            }
        } else {
            tracing::warn!(
                model = spec.filename,
                sha256 = %digest,
                "downloaded model has no pinned checksum; pin this digest to enable verification"
            );
        }
        // Atomic-ish write: temp then rename.
        let final_path = spec.local_path();
        let tmp = final_path.with_extension("onnx.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &final_path)?;
        Ok(digest)
    }
}

#[cfg(feature = "onnx")]
pub use onnx_impl::{download_model, OnnxRemover};

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny 2x2 red PNG in memory.
    fn tiny_png() -> Vec<u8> {
        let mut img = image::RgbaImage::new(2, 2);
        for p in img.pixels_mut() {
            *p = image::Rgba([200, 40, 40, 255]);
        }
        encode_png_rgba(&img).unwrap()
    }

    #[test]
    fn passthrough_produces_valid_rgba_png() {
        let png = tiny_png();
        let out = PassthroughRemover.remove_background(&png).unwrap();
        let decoded = image::load_from_memory(&out).unwrap();
        assert_eq!(decoded.color(), image::ColorType::Rgba8);
        assert_eq!(decoded.width(), 2);
        assert_eq!(PassthroughRemover.label(), "passthrough");
    }

    #[test]
    fn sha256_is_stable_hex() {
        let d = sha256_hex(b"duduclaw");
        assert_eq!(d.len(), 64);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic.
        assert_eq!(d, sha256_hex(b"duduclaw"));
    }

    #[test]
    fn model_specs_resolve_under_models_dir() {
        assert!(BIREFNET_LITE
            .local_path()
            .ends_with("models/birefnet-general-lite.onnx"));
        assert_eq!(SILUETA.input_size, 320);
    }
}
