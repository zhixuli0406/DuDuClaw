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
        let img = apply_exif_orientation(img, image_bytes);
        encode_png_rgba(&img.to_rgba8())
    }
    fn label(&self) -> &'static str {
        "passthrough"
    }
}

/// Apply the EXIF `Orientation` tag (phone photos): the `image` decoder keeps
/// raw sensor orientation, so portrait shots arrive rotated. No/invalid EXIF →
/// the image is returned untouched.
pub(crate) fn apply_exif_orientation(
    img: image::DynamicImage,
    raw_bytes: &[u8],
) -> image::DynamicImage {
    let orientation = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(raw_bytes))
        .ok()
        .and_then(|meta| {
            meta.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1);
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
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
/// pinned SHA-256, and the square input side the net expects.
pub struct ModelSpec {
    pub variant: ModelVariant,
    /// Filename under `~/.duduclaw/models/`.
    pub filename: &'static str,
    /// HTTPS source URL.
    pub url: &'static str,
    /// Pinned lowercase hex SHA-256 of the artifact at `url`. `download_model`
    /// verifies fail-closed: a mismatch discards the bytes and errors — a
    /// corrupted or tampered model is never written to disk. Every spec MUST
    /// ship a real pinned digest (never fabricate one).
    pub expected_sha256: &'static str,
    /// Square input side (px) the network expects.
    pub input_size: u32,
}

/// BiRefNet-general-lite spec.
///
/// SHA-256 pinned 2026-07-28 from the live upstream URL (224,005,088 bytes);
/// a fresh download and the operator's existing `~/.duduclaw/models/` copy
/// produced the identical digest.
pub const BIREFNET_LITE: ModelSpec = ModelSpec {
    variant: ModelVariant::Birefnet,
    filename: "birefnet-general-lite.onnx",
    url: "https://huggingface.co/onnx-community/BiRefNet_lite/resolve/main/onnx/model.onnx",
    expected_sha256: "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333",
    input_size: 1024,
};

/// silueta spec (rembg release artifact).
///
/// SHA-256 pinned 2026-07-28 from the live upstream URL (44,173,029 bytes);
/// a fresh download and the operator's existing `~/.duduclaw/models/` copy
/// produced the identical digest. The rembg `v0.0.0` release tag is the
/// project's stable model-hosting release, not a moving branch.
pub const SILUETA: ModelSpec = ModelSpec {
    variant: ModelVariant::Silueta,
    filename: "silueta.onnx",
    url: "https://github.com/danielgatis/rembg/releases/download/v0.0.0/silueta.onnx",
    expected_sha256: "75da6c8d2f8096ec743d071951be73b4a8bc7b3e51d9a6625d63644f90ffeedb",
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

/// Verify `bytes` against a pinned lowercase-hex SHA-256.
///
/// Returns the computed digest on match. On mismatch returns
/// [`PetError::ModelDownload`] carrying both digests — fail-closed: the caller
/// must discard the bytes and never install them as a model file.
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<String> {
    let digest = sha256_hex(bytes);
    if digest.eq_ignore_ascii_case(expected) {
        Ok(digest)
    } else {
        Err(PetError::ModelDownload(format!(
            "checksum mismatch: expected {expected}, got {digest}"
        )))
    }
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

            let decoded = image::load_from_memory(image_bytes)
                .map_err(|e| PetError::Image(format!("decode failed: {e}")))?;
            // Phone photos carry EXIF orientation the decoder does not apply —
            // without this the whole pipeline runs on a rotated image.
            let orig = apply_exif_orientation(decoded, image_bytes).to_rgb8();
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

            // BiRefNet exports can emit SEVERAL maps (coarse decoder stages +
            // the final refined matte). Grabbing the first map yields a blurry
            // vignette instead of a segmentation — pick the output with the
            // largest spatial area; on ties prefer the LAST (refinement order).
            let mut best: Option<(String, Vec<usize>, Vec<f32>)> = None;
            for (name, value) in outputs.iter() {
                let Ok((s, d)) = value.try_extract_tensor::<f32>() else {
                    continue;
                };
                let dims: Vec<usize> = s.iter().map(|d| *d as usize).collect();
                let area = if dims.len() >= 2 {
                    dims[dims.len() - 2] * dims[dims.len() - 1]
                } else {
                    0
                };
                let better = match &best {
                    None => true,
                    Some((_, bdims, _)) => {
                        let barea = bdims[bdims.len() - 2] * bdims[bdims.len() - 1];
                        area >= barea
                    }
                };
                if better {
                    best = Some((name.to_string(), dims, d.to_vec()));
                }
            }
            let (out_name, shape, flat) =
                best.ok_or_else(|| PetError::Inference("no f32 output tensor".into()))?;
            tracing::debug!(output = %out_name, ?shape, "segmentation output selected");

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

            // Map scores to [0,1] probabilities. Logit-range outputs go through
            // a sigmoid (min-max normalisation instead turns mid-tones into a
            // translucent veil over the whole photo); already-probabilistic
            // outputs are clamped as-is.
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for &v in mask {
                lo = lo.min(v);
                hi = hi.max(v);
            }
            let is_logits = lo < -0.05 || hi > 1.05;
            let to_prob = |v: f32| -> f32 {
                if is_logits {
                    1.0 / (1.0 + (-v).exp())
                } else {
                    v.clamp(0.0, 1.0)
                }
            };

            // Rasterize the mask to a grayscale image, then resize to the original.
            let mut mask_img = image::GrayImage::new(mw as u32, mh as u32);
            for (i, &v) in mask.iter().enumerate() {
                let a = (to_prob(v) * 255.0).clamp(0.0, 255.0) as u8;
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

    /// Download a model to `~/.duduclaw/models/` and verify its pinned SHA-256.
    ///
    /// Fail-closed: the downloaded bytes are hashed and checked against
    /// `spec.expected_sha256` BEFORE anything is written under the models dir —
    /// on mismatch the bytes are dropped, any stale temp file is removed, and a
    /// hard error is returned. A corrupted or tampered download can never
    /// become a loadable model file. Never downloads at build time — this is
    /// an explicit runtime action.
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
        // Verify BEFORE writing anything — bad bytes must never land on disk.
        let final_path = spec.local_path();
        let tmp = final_path.with_extension("onnx.tmp");
        let digest = match verify_sha256(&bytes, spec.expected_sha256) {
            Ok(d) => d,
            Err(e) => {
                // Clean up any stale temp from an earlier interrupted attempt.
                let _ = std::fs::remove_file(&tmp);
                return Err(PetError::ModelDownload(format!(
                    "{} from {}: {e}",
                    spec.filename, spec.url
                )));
            }
        };
        // Atomic-ish write: temp then rename.
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

    #[test]
    fn verify_sha256_accepts_matching_digest() {
        // sha256("duduclaw") computed by the same helper — the pass path only
        // asserts the round trip: digest(x) verifies against itself, and case
        // of the pinned hex must not matter.
        let expected = sha256_hex(b"duduclaw");
        let got = verify_sha256(b"duduclaw", &expected).unwrap();
        assert_eq!(got, expected);
        // Uppercase pin still verifies (eq_ignore_ascii_case).
        let upper = expected.to_ascii_uppercase();
        assert!(verify_sha256(b"duduclaw", &upper).is_ok());
    }

    #[test]
    fn verify_sha256_rejects_mismatch_fail_closed() {
        let pinned = sha256_hex(b"the real model");
        let err = verify_sha256(b"tampered bytes", &pinned).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("checksum mismatch"), "got: {msg}");
        // The error names both digests so the operator can diagnose.
        assert!(msg.contains(&pinned), "got: {msg}");
    }

    #[test]
    fn model_spec_pins_are_wellformed_sha256() {
        for spec in [&BIREFNET_LITE, &SILUETA] {
            assert_eq!(spec.expected_sha256.len(), 64, "{}", spec.filename);
            assert!(
                spec.expected_sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{} pin must be lowercase hex",
                spec.filename
            );
        }
        // The two models are distinct artifacts — identical pins would mean a
        // copy-paste error.
        assert_ne!(BIREFNET_LITE.expected_sha256, SILUETA.expected_sha256);
    }
}
