//! Local embedding through ONNX Runtime.
//!
//! Loads lazily on first use (shared one-time ORT init in `duduclaw-onnx`),
//! serialises inference behind a mutex (`Session::run` needs `&mut`), and
//! drops the session after an idle window ([`LocalOnnxEmbedder::unload_if_idle`]).
//!
//! Inputs are built from the graph's own input names: `input_ids`,
//! `attention_mask`, `token_type_ids` (zeros), `position_ids` (positions of
//! real tokens) and, for decoder exports, an empty `past_key_values.*` cache
//! of shape `[batch, kv_heads, 0, head_dim]` (a no-cache forward pass).
//! Unknown inputs refuse to run. Texts are batched after sorting by length
//! so padding stays small; results come back in input order.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ort::memory::Allocator;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::Tokenizer;

use super::catalog::{ModelSpec, Pooling};
use super::pooling;
use super::{Egress, EmbedError, Embedder};

struct Loaded {
    session: Session,
    tokenizer: Tokenizer,
    input_names: Vec<String>,
    output_index: usize,
    last_used: Instant,
}

/// A catalog model running on this machine.
pub struct LocalOnnxEmbedder {
    spec: ModelSpec,
    model_dir: PathBuf,
    ort_lib: PathBuf,
    threads: usize,
    batch_size: usize,
    model_id: String,
    loaded: Mutex<Option<Loaded>>,
}

impl std::fmt::Debug for LocalOnnxEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalOnnxEmbedder")
            .field("model", &self.spec.id)
            .field("threads", &self.threads)
            .finish()
    }
}

fn failed(e: impl std::fmt::Display) -> EmbedError {
    EmbedError::Failed(format!("向量模型執行失敗：{e}"))
}

impl LocalOnnxEmbedder {
    /// An embedder for `spec` installed under `home`. Nothing is loaded
    /// until the first call; the caller checks
    /// [`super::install::installed_problem`] first.
    pub fn new(
        spec: &ModelSpec,
        home: &Path,
        ort_lib: PathBuf,
        threads: usize,
        batch_size: usize,
    ) -> Self {
        Self {
            model_id: spec.model_id(),
            model_dir: spec.install_dir(home),
            spec: spec.clone(),
            ort_lib,
            threads: threads.max(1),
            batch_size: batch_size.clamp(1, 64).min(spec.batch_limit.max(1)),
            loaded: Mutex::new(None),
        }
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    fn load(&self) -> Result<Loaded, EmbedError> {
        let t0 = Instant::now();
        duduclaw_onnx::runtime::init_ort(&self.ort_lib).map_err(EmbedError::Unavailable)?;
        let mut tokenizer = Tokenizer::from_file(self.model_dir.join(&self.spec.tokenizer_file))
            .map_err(|e| EmbedError::Unavailable(format!("載入向量模型的 tokenizer 失敗：{e}")))?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: self.spec.max_tokens,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Unavailable(format!("tokenizer 設定失敗：{e}")))?;
        tokenizer.with_padding(None);
        let graph = self.model_dir.join(&self.spec.model_file);
        let threads = self.threads;
        let build = || -> Result<Session, String> {
            let b = Session::builder().map_err(|e| e.to_string())?;
            let b = b
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| e.to_string())?;
            let mut b = b.with_intra_threads(threads).map_err(|e| e.to_string())?;
            b.commit_from_file(&graph).map_err(|e| e.to_string())
        };
        let session =
            build().map_err(|e| EmbedError::Unavailable(format!("載入向量模型失敗：{e}")))?;
        let input_names: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        for name in &input_names {
            if !known_input(name) {
                return Err(EmbedError::Unavailable(format!(
                    "向量模型需要未知的輸入 `{name}`，這個版本的 DuDuClaw 不支援"
                )));
            }
        }
        let output_index = session
            .outputs()
            .iter()
            .position(|o| o.name() == "last_hidden_state")
            .unwrap_or(0);
        tracing::info!(
            model = %self.spec.id,
            ms = t0.elapsed().as_millis(),
            threads,
            inputs = input_names.len(),
            "knowledge: embedding model loaded"
        );
        Ok(Loaded {
            session,
            tokenizer,
            input_names,
            output_index,
            last_used: Instant::now(),
        })
    }

    /// Drop the session after `window` without use. Returns true when it
    /// unloaded.
    pub fn unload_if_idle(&self, window: Duration) -> bool {
        let mut g = self.loaded.lock().unwrap_or_else(|p| p.into_inner());
        if g.as_ref().is_some_and(|l| l.last_used.elapsed() >= window) {
            *g = None;
            tracing::info!(model = %self.spec.id, "knowledge: embedding model unloaded after idle window");
            return true;
        }
        false
    }

    /// Whether a session is resident.
    pub fn is_loaded(&self) -> bool {
        self.loaded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    fn embed_inputs(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut guard = self.loaded.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(self.load()?);
        }
        let loaded = guard.as_mut().ok_or_else(|| failed("session missing"))?;
        // Sort by length so each batch pads to a similar size.
        let mut order: Vec<usize> = (0..inputs.len()).collect();
        order.sort_by_key(|&i| inputs[i].chars().count());
        let mut out: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
        for batch in order.chunks(self.batch_size) {
            let texts: Vec<&str> = batch.iter().map(|&i| inputs[i].as_str()).collect();
            let vectors = run_batch(loaded, &self.spec, &texts)?;
            for (&i, v) in batch.iter().zip(vectors) {
                out[i] = Some(v);
            }
        }
        loaded.last_used = Instant::now();
        out.into_iter()
            .map(|v| v.ok_or_else(|| failed("missing output row")))
            .collect()
    }
}

fn known_input(name: &str) -> bool {
    matches!(
        name,
        "input_ids" | "attention_mask" | "token_type_ids" | "position_ids"
    ) || (name.starts_with("past_key_values.")
        && (name.ends_with(".key") || name.ends_with(".value")))
}

/// `position_ids` for right-padded rows: 0.. over real tokens, 1 on padding
/// (the convention Hugging Face uses; padded positions are masked anyway).
fn position_ids(mask: &[i64], batch: usize, seq: usize) -> Vec<i64> {
    let mut out = vec![1i64; batch * seq];
    for b in 0..batch {
        let mut pos = 0i64;
        for t in 0..seq {
            if mask[b * seq + t] != 0 {
                out[b * seq + t] = pos;
                pos += 1;
            }
        }
    }
    out
}

fn run_batch(
    loaded: &mut Loaded,
    spec: &ModelSpec,
    texts: &[&str],
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let encodings = loaded
        .tokenizer
        .encode_batch(texts.to_vec(), true)
        .map_err(|e| failed(format!("tokenize：{e}")))?;
    let batch = encodings.len();
    let seq = encodings
        .iter()
        .map(|e| e.get_ids().len())
        .max()
        .unwrap_or(0)
        .max(1);
    let mut ids = vec![0i64; batch * seq];
    let mut mask = vec![0i64; batch * seq];
    for (b, enc) in encodings.iter().enumerate() {
        for (t, (&id, &m)) in enc
            .get_ids()
            .iter()
            .zip(enc.get_attention_mask())
            .enumerate()
        {
            ids[b * seq + t] = id as i64;
            mask[b * seq + t] = m as i64;
        }
    }
    let shape = vec![batch as i64, seq as i64];
    let mut feeds: Vec<(Cow<'_, str>, SessionInputValue<'_>)> =
        Vec::with_capacity(loaded.input_names.len());
    for name in &loaded.input_names {
        let value: SessionInputValue<'_> = if name.starts_with("past_key_values.") {
            // `from_array` refuses a zero dimension; an allocator-backed
            // tensor of shape [batch, kv_heads, 0, head_dim] is the empty cache.
            let kv_shape = vec![batch as i64, spec.kv_heads as i64, 0, spec.head_dim as i64];
            Tensor::<f32>::new(&Allocator::default(), kv_shape)
                .map_err(failed)?
                .into()
        } else {
            let data = match name.as_str() {
                "input_ids" => ids.clone(),
                "attention_mask" => mask.clone(),
                "token_type_ids" => vec![0i64; batch * seq],
                "position_ids" => position_ids(&mask, batch, seq),
                other => return Err(failed(format!("unknown input {other}"))),
            };
            Tensor::<i64>::from_array((shape.clone(), data))
                .map_err(failed)?
                .into()
        };
        feeds.push((Cow::Owned(name.clone()), value));
    }
    let outputs = loaded.session.run(feeds).map_err(failed)?;
    let (out_shape, data) = outputs[loaded.output_index]
        .try_extract_tensor::<f32>()
        .map_err(failed)?;
    if out_shape.len() != 3 || out_shape[0] as usize != batch || out_shape[1] as usize != seq {
        return Err(failed(format!(
            "unexpected output shape {:?}",
            &out_shape[..]
        )));
    }
    let hidden = out_shape[2] as usize;
    if hidden != spec.native_dim {
        return Err(failed(format!(
            "hidden size {hidden} ≠ {}",
            spec.native_dim
        )));
    }
    let pooled = match spec.pooling {
        Pooling::Mean => pooling::mean_pool(data, &mask, batch, seq, hidden),
        Pooling::LastToken => pooling::last_token_pool(data, &mask, batch, seq, hidden),
    };
    Ok(pooled
        .into_iter()
        .map(|v| pooling::truncate_and_normalize(v, spec.dim))
        .collect())
}

impl Embedder for LocalOnnxEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.spec.dim
    }

    fn max_input_tokens(&self) -> usize {
        self.spec.max_tokens
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.embed_inputs(texts.iter().map(|t| self.spec.passage_input(t)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.embed_inputs(vec![self.spec.query_input(text)])?
            .pop()
            .ok_or_else(|| failed("empty result"))
    }

    fn egress(&self) -> Egress {
        Egress::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_ids_count_real_tokens_only() {
        let mask = [1, 1, 1, 0, 1, 0, 0, 0];
        assert_eq!(position_ids(&mask, 2, 4), vec![0, 1, 2, 1, 0, 1, 1, 1]);
    }

    #[test]
    fn only_known_inputs_are_accepted() {
        for ok in [
            "input_ids",
            "attention_mask",
            "token_type_ids",
            "position_ids",
            "past_key_values.0.key",
            "past_key_values.27.value",
        ] {
            assert!(known_input(ok), "{ok}");
        }
        for bad in ["pixel_values", "past_key_values.0", "use_cache_branch"] {
            assert!(!known_input(bad), "{bad}");
        }
    }

    #[test]
    fn missing_runtime_is_unavailable_not_a_crash() {
        let spec = super::super::catalog::find(super::super::catalog::DEFAULT_MODEL).unwrap();
        let home = tempfile::tempdir().unwrap();
        let e = LocalOnnxEmbedder::new(spec, home.path(), home.path().join("no-lib.dylib"), 1, 4);
        assert!(matches!(
            e.embed_query("特休"),
            Err(EmbedError::Unavailable(_))
        ));
        assert!(!e.is_loaded());
    }
}
