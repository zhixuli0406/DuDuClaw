//! UCCI router files fitted offline on human-checked outcomes.

use std::path::{Path, PathBuf};

use serde::Serialize;
use ucci::{Router, policy::CostModel};

use crate::config::RouterConfig;
use crate::router::RoutingTier;
use crate::types::InferenceResponse;

pub struct UcciCascade {
    fast: Option<Router>,
    strong: Option<Router>,
    observations: Option<PathBuf>,
    requested: bool,
    fast_requested: bool,
    strong_requested: bool,
}

impl UcciCascade {
    pub fn load(config: &RouterConfig, home: &Path) -> Self {
        let fast = config
            .ucci_fast_router
            .as_deref()
            .and_then(|p| load_router(p, home));
        let strong = config
            .ucci_strong_router
            .as_deref()
            .and_then(|p| load_router(p, home));
        let observations = config
            .ucci_observations
            .as_deref()
            .map(|p| resolve(p, home));
        Self {
            fast,
            strong,
            observations,
            requested: config.ucci_fast_router.is_some() || config.ucci_strong_router.is_some(),
            fast_requested: config.ucci_fast_router.is_some(),
            strong_requested: config.ucci_strong_router.is_some(),
        }
    }

    pub fn requested(&self) -> bool {
        self.requested
    }

    pub fn collecting(&self) -> bool {
        self.observations.is_some()
    }

    pub fn router(&self, tier: RoutingTier) -> Option<&Router> {
        match tier {
            RoutingTier::LocalFast => self.fast.as_ref(),
            RoutingTier::LocalStrong => self.strong.as_ref(),
            RoutingTier::CloudApi => None,
        }
    }

    pub fn stage_configured(&self, tier: RoutingTier) -> bool {
        match tier {
            RoutingTier::LocalFast => self.fast.is_some() || self.fast_requested,
            RoutingTier::LocalStrong => self.strong.is_some() || self.strong_requested,
            RoutingTier::CloudApi => false,
        }
    }

    /// Append one reviewable row to the observation JSONL.
    ///
    /// The append holds `duduclaw_core::with_file_lock` (coding convention 3):
    /// a per-instance in-process mutex could not serialize a second
    /// `UcciCascade` (another agent's engine, or a hot reload) pointed at the
    /// same path, and a row over `PIPE_BUF` would then interleave — corrupting
    /// the very file `scripts/ucci_fit.py` fits on. The lock is blocking, so
    /// the whole append runs on a blocking thread.
    pub async fn observe(&self, observation: &Observation<'_>) {
        let Some(path) = self.observations.as_ref() else {
            return;
        };
        let row = match serde_json::to_vec(observation) {
            Ok(mut row) => {
                row.push(b'\n');
                row
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "UCCI observation was not saved");
                return;
            }
        };
        let log_path = path.clone();
        let path = path.clone();
        let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            duduclaw_core::with_file_lock(&path, || {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                file.write_all(&row)?;
                file.flush()
            })
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, path = %log_path.display(), "UCCI observation was not saved")
            }
            Err(error) => {
                tracing::warn!(%error, path = %log_path.display(), "UCCI observation task failed")
            }
        }
    }
}

fn resolve(path: &str, home: &Path) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        home.join(path)
    }
}

fn load_router(path: &str, home: &Path) -> Option<Router> {
    let path = resolve(path, home);
    match Router::load(&path) {
        Ok(router) if router.costs().model == CostModel::Sequential => Some(router),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "UCCI router must use the sequential cost model");
            None
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "UCCI router could not be loaded");
            None
        }
    }
}

/// A reviewable candidate row. Accuracy labels must be added by a human before
/// fitting; automatic judge outcomes are not treated as ground truth.
#[derive(Serialize)]
pub struct Observation<'a> {
    pub id: String,
    pub request_id: &'a str,
    pub stage: RoutingTier,
    pub model_id: &'a str,
    pub system_prompt: &'a str,
    pub prompt: &'a str,
    pub answer: &'a str,
    pub u: Option<f64>,
    pub mean_logprob: Option<f32>,
    pub p_hat: Option<f64>,
    pub escalated: Option<bool>,
    pub generation_time_ms: u64,
    pub small_correct: Option<bool>,
    pub large_correct: Option<bool>,
    pub label_source: Option<&'a str>,
}

impl<'a> Observation<'a> {
    pub fn from_response(
        request_id: &'a str,
        stage: RoutingTier,
        system_prompt: &'a str,
        prompt: &'a str,
        response: &'a InferenceResponse,
        p_hat: Option<f64>,
        escalated: Option<bool>,
    ) -> Self {
        Self {
            id: format!("{request_id}-{stage}"),
            request_id,
            stage,
            model_id: &response.model_id,
            system_prompt,
            prompt,
            answer: &response.text,
            u: response.margin_uncertainty,
            mean_logprob: response.mean_logprob,
            p_hat,
            escalated,
            generation_time_ms: response.generation_time_ms,
            small_correct: None,
            large_correct: None,
            label_source: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ucci::{calibration::IsotonicCalibrator, policy::Costs};

    #[test]
    fn loads_only_sequential_router_for_each_stage() {
        let dir = tempfile::tempdir().unwrap();
        let calibrator = IsotonicCalibrator::fit(&[0.0, 1.0], &[0.0, 1.0]).unwrap();
        let sequential = Router::new(
            calibrator.clone(),
            0.5,
            Costs::new(1.0, 3.0, CostModel::Sequential).unwrap(),
        )
        .unwrap();
        sequential.save(dir.path().join("fast.json")).unwrap();
        let routing = Router::new(
            calibrator,
            0.5,
            Costs::new(1.0, 3.0, CostModel::Routing).unwrap(),
        )
        .unwrap();
        routing.save(dir.path().join("strong.json")).unwrap();
        let config = RouterConfig {
            ucci_fast_router: Some("fast.json".to_string()),
            ucci_strong_router: Some("strong.json".to_string()),
            ..RouterConfig::default()
        };
        let cascade = UcciCascade::load(&config, dir.path());
        assert!(
            cascade
                .router(RoutingTier::LocalFast)
                .unwrap()
                .route(0.8)
                .unwrap()
                .escalate
        );
        assert!(cascade.router(RoutingTier::LocalStrong).is_none());
    }

    /// Regression: `observe()` used a per-instance `tokio::Mutex`, which is not
    /// a lock any other writer can see. This asserts the advisory file lock is
    /// really held across the append: while a second holder (another process,
    /// or another `UcciCascade` in this one) owns
    /// `duduclaw_core::with_file_lock` on the same path, no observation may
    /// reach the file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn observe_waits_for_the_shared_advisory_lock() {
        const FILLER: &str = "本機觀測";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("observations.jsonl");
        let config = RouterConfig {
            ucci_observations: Some("observations.jsonl".to_string()),
            ..RouterConfig::default()
        };
        let cascade = std::sync::Arc::new(UcciCascade::load(&config, dir.path()));

        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let locked_path = path.clone();
        let holder = std::thread::spawn(move || {
            duduclaw_core::with_file_lock(&locked_path, || {
                held_tx.send(()).unwrap();
                let _ = release_rx.recv();
                Ok(())
            })
            .unwrap();
        });
        held_rx.recv().unwrap();

        let writing = tokio::spawn(async move {
            cascade
                .observe(&Observation {
                    id: "blocked".to_string(),
                    request_id: "blocked",
                    stage: RoutingTier::LocalFast,
                    model_id: "test-model",
                    system_prompt: FILLER,
                    prompt: FILLER,
                    answer: FILLER,
                    u: Some(0.25),
                    mean_logprob: Some(-0.5),
                    p_hat: Some(0.75),
                    escalated: Some(false),
                    generation_time_ms: 1,
                    small_correct: None,
                    large_correct: None,
                    label_source: None,
                })
                .await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let while_locked = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            while_locked.is_empty(),
            "observe() wrote while another holder owned the advisory lock"
        );
        assert!(!writing.is_finished());

        release_tx.send(()).unwrap();
        holder.join().unwrap();
        writing.await.unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.lines().count() == 1 && after.ends_with('\n'),
            "the row must land in full once the lock is released"
        );
    }

    /// Companion invariant: two instances writing the same file concurrently
    /// must leave one whole JSON object per line. NOTE (honest): on macOS a
    /// single `write_all` of a regular file in append mode is effectively
    /// atomic, so this test does NOT fail against the old lock-free code here —
    /// it locks in the invariant, while
    /// `observe_waits_for_the_shared_advisory_lock` above is the test that
    /// actually catches the missing lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_cascades_append_whole_observation_lines() {
        let dir = tempfile::tempdir().unwrap();
        let config = RouterConfig {
            ucci_observations: Some("observations.jsonl".to_string()),
            ..RouterConfig::default()
        };
        // Two independent instances: the old in-process mutex guarded neither
        // against the other.
        let first = std::sync::Arc::new(UcciCascade::load(&config, dir.path()));
        let second = std::sync::Arc::new(UcciCascade::load(&config, dir.path()));
        assert!(first.collecting());

        // Well past PIPE_BUF (512B–4KiB depending on platform) so a lock-free
        // append would tear.
        let filler = "本機觀測".repeat(4096);
        let rows_per_writer = 12;
        let mut tasks = Vec::new();
        for (writer, cascade) in [first.clone(), second.clone()].into_iter().enumerate() {
            let filler = filler.clone();
            tasks.push(tokio::spawn(async move {
                for row in 0..rows_per_writer {
                    let id = format!("w{writer}-r{row}");
                    cascade
                        .observe(&Observation {
                            id: id.clone(),
                            request_id: &id,
                            stage: RoutingTier::LocalFast,
                            model_id: "test-model",
                            system_prompt: &filler,
                            prompt: &filler,
                            answer: &filler,
                            u: Some(0.25),
                            mean_logprob: Some(-0.5),
                            p_hat: Some(0.75),
                            escalated: Some(false),
                            generation_time_ms: 1,
                            small_correct: None,
                            large_correct: None,
                            label_source: None,
                        })
                        .await;
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let written = std::fs::read_to_string(dir.path().join("observations.jsonl")).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), rows_per_writer * 2, "every row must land once");
        for line in lines {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("interleaved/torn JSONL row: {error}"));
            assert!(parsed.get("id").and_then(|id| id.as_str()).is_some());
        }
    }
}
