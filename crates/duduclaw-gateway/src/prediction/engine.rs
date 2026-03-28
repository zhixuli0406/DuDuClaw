//! Core prediction engine — generates predictions, calculates errors, manages models.
//!
//! The engine maintains per-(user, agent) statistical models and uses them to
//! predict conversation outcomes. Prediction errors drive the evolution system:
//! large errors trigger deep reflection, small errors require zero LLM cost.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::metacognition::MetaCognition;
use super::metrics::ConversationMetrics;
use super::user_model::UserModel;

// ---------------------------------------------------------------------------
// Prediction + Error types
// ---------------------------------------------------------------------------

/// A prediction about what will happen in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    /// Expected user satisfaction (0.0 - 1.0).
    pub expected_satisfaction: f64,
    /// Expected follow-up question rate (0.0 - 1.0).
    pub expected_follow_up_rate: f64,
    /// Predicted dominant topic (if enough data).
    pub expected_topic: Option<String>,
    /// Confidence in this prediction (0.0 = cold start, 1.0 = mature model).
    pub confidence: f64,
    /// When this prediction was made.
    pub timestamp: DateTime<Utc>,
}

/// Category of prediction error — determines the evolution response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCategory {
    /// composite_error < threshold_negligible: prediction accurate, just update stats.
    Negligible,
    /// moderate range: record as episodic memory, no LLM.
    Moderate,
    /// significant range: trigger LLM reflection.
    Significant,
    /// composite_error >= threshold_critical: emergency evolution.
    Critical,
}

/// The measured gap between prediction and reality.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionError {
    /// Gap in satisfaction: predicted - inferred actual.
    pub delta_satisfaction: f64,
    /// How surprising the conversation topic was (0.0 - 1.0).
    pub topic_surprise: f64,
    /// User corrected the agent when we didn't predict it.
    pub unexpected_correction: bool,
    /// User had many follow-ups when we predicted few.
    pub unexpected_follow_up: bool,
    /// Weighted combination of all error signals.
    pub composite_error: f64,
    /// Classification based on composite_error magnitude.
    pub category: ErrorCategory,
    /// The original prediction.
    pub prediction: Prediction,
    /// The actual conversation metrics.
    pub actual: ConversationMetrics,
}

// ---------------------------------------------------------------------------
// PredictionEngine
// ---------------------------------------------------------------------------

/// Core engine: manages user models, generates predictions, calculates errors.
pub struct PredictionEngine {
    /// In-memory cache of user models: (user_id, agent_id) -> model.
    models: Arc<Mutex<HashMap<(String, String), UserModel>>>,
    /// SQLite database path for persistence.
    db_path: PathBuf,
    /// Recent error history per agent (ring buffer, last 10).
    consecutive_errors: Arc<Mutex<HashMap<String, Vec<ErrorCategory>>>>,
    /// Self-calibrating metacognition system.
    pub metacognition: Arc<Mutex<MetaCognition>>,
    /// How often to save models (every N updates).
    save_interval: u64,
    /// Updates since last save, per model key.
    update_counts: Arc<Mutex<HashMap<(String, String), u64>>>,
}

impl PredictionEngine {
    /// Create a new prediction engine, initializing SQLite tables and loading cached models.
    pub fn new(db_path: PathBuf, meta_path: Option<PathBuf>) -> Self {
        // Initialize database tables
        if let Ok(conn) = Connection::open(&db_path) {
            if let Err(e) = Self::init_tables(&conn) {
                warn!("Failed to init prediction tables: {e}");
            }
        }

        // Load metacognition state
        let metacognition = meta_path
            .as_ref()
            .and_then(|p| MetaCognition::load(p))
            .unwrap_or_default();

        let engine = Self {
            models: Arc::new(Mutex::new(HashMap::new())),
            db_path,
            consecutive_errors: Arc::new(Mutex::new(HashMap::new())),
            metacognition: Arc::new(Mutex::new(metacognition)),
            save_interval: 5,
            update_counts: Arc::new(Mutex::new(HashMap::new())),
        };

        // Load existing models from disk
        if let Err(e) = engine.load_all_models() {
            warn!("Failed to load user models from disk: {e}");
        }

        engine
    }

    fn init_tables(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_models (
                user_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                model_json TEXT NOT NULL,
                total_conversations INTEGER DEFAULT 0,
                last_updated TEXT NOT NULL,
                PRIMARY KEY (user_id, agent_id)
            );
            CREATE INDEX IF NOT EXISTS idx_user_models_agent
                ON user_models(agent_id);

            CREATE TABLE IF NOT EXISTS prediction_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                composite_error REAL NOT NULL,
                category TEXT NOT NULL,
                timestamp TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_prediction_log_agent
                ON prediction_log(agent_id);"
        ).map_err(|e| e.to_string())
    }

    fn load_all_models(&self) -> Result<(), String> {
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT user_id, agent_id, model_json FROM user_models")
            .map_err(|e| e.to_string())?;

        let mut models = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                let user_id: String = row.get(0)?;
                let agent_id: String = row.get(1)?;
                let json: String = row.get(2)?;
                Ok((user_id, agent_id, json))
            })
            .map_err(|e| e.to_string())?;

        for row in rows {
            if let Ok((user_id, agent_id, json)) = row {
                if let Ok(model) = serde_json::from_str::<UserModel>(&json) {
                    models.insert((user_id, agent_id), model);
                }
            }
        }

        let count = models.len();
        // Safe: this is called from the constructor before any async tasks
        // can access the engine, so the mutex is uncontested.
        // Use try_lock to avoid blocking_lock panic in async context.
        if let Ok(mut guard) = self.models.try_lock() {
            *guard = models;
        } else {
            warn!("Could not acquire models lock at startup — models not preloaded");
        }
        if count > 0 {
            info!(count, "Loaded user models from disk");
        }
        Ok(())
    }

    /// Generate a prediction for an upcoming conversation.
    ///
    /// This is a pure statistical operation — zero LLM cost, < 1ms.
    pub async fn predict(&self, user_id: &str, agent_id: &str, _message: &str) -> Prediction {
        let models = self.models.lock().await;
        let key = (user_id.to_string(), agent_id.to_string());

        let now = Utc::now();

        if let Some(model) = models.get(&key) {
            let top_topic = model
                .topic_distribution
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(k, _)| k.clone());

            Prediction {
                expected_satisfaction: if model.avg_satisfaction.sample_count() > 0 {
                    model.avg_satisfaction.mean().clamp(0.0, 1.0)
                } else {
                    0.7 // optimistic default
                },
                expected_follow_up_rate: model.follow_up_rate.mean().clamp(0.0, 1.0),
                expected_topic: top_topic,
                confidence: model.confidence(),
                timestamp: now,
            }
        } else {
            // Cold start: return neutral prediction with low confidence
            Prediction {
                expected_satisfaction: 0.7,
                expected_follow_up_rate: 0.3,
                expected_topic: None,
                confidence: 0.0,
                timestamp: now,
            }
        }
    }

    /// Calculate the prediction error from actual conversation metrics.
    ///
    /// Zero LLM cost — pure arithmetic.
    pub async fn calculate_error(
        &self,
        prediction: &Prediction,
        actual: &ConversationMetrics,
    ) -> PredictionError {
        // Infer actual satisfaction from behavioural signals
        let mut inferred_satisfaction: f64 = 0.7; // neutral baseline
        inferred_satisfaction -= actual.user_corrections as f64 * 0.3;
        inferred_satisfaction -= (actual.user_follow_ups.saturating_sub(1)) as f64 * 0.1;

        if let Some(ref feedback) = actual.feedback_signal {
            match feedback.as_str() {
                "positive" => inferred_satisfaction += 0.2,
                "negative" => inferred_satisfaction -= 0.4,
                "correction" => inferred_satisfaction -= 0.3,
                _ => {}
            }
        }
        let inferred_satisfaction = inferred_satisfaction.clamp(0.0, 1.0);

        let delta_satisfaction = prediction.expected_satisfaction - inferred_satisfaction;

        // Topic surprise: partial matching via keyword overlap (0.0 - 1.0).
        // Exact match → 0.0; partial overlap → scaled; no overlap → 0.7.
        let topic_surprise = if let Some(ref expected) = prediction.expected_topic {
            if actual.extracted_topics.iter().any(|t| t == expected) {
                0.0 // exact match
            } else {
                // Check partial overlap: does any actual topic share characters with expected?
                let best_overlap = actual.extracted_topics.iter().map(|t| {
                    let expected_chars: std::collections::HashSet<char> = expected.chars().collect();
                    let topic_chars: std::collections::HashSet<char> = t.chars().collect();
                    let inter = expected_chars.intersection(&topic_chars).count() as f64;
                    let union = expected_chars.union(&topic_chars).count().max(1) as f64;
                    inter / union
                }).fold(0.0_f64, f64::max);

                // Scale: 0 overlap → 0.7, full overlap → 0.0
                (1.0 - best_overlap) * 0.7
            }
        } else {
            0.0 // no prediction → no surprise
        };

        let unexpected_correction =
            prediction.expected_satisfaction > 0.6 && actual.user_corrections > 0;

        let unexpected_follow_up = prediction.expected_follow_up_rate < 0.3
            && actual.user_follow_ups > 2;

        // Weighted composite error
        let composite_error = (0.40 * delta_satisfaction.abs()
            + 0.20 * topic_surprise
            + 0.20 * if unexpected_correction { 1.0 } else { 0.0 }
            + 0.20 * if unexpected_follow_up { 1.0 } else { 0.0 })
        .clamp(0.0, 1.0);

        // Classify using metacognition's adaptive thresholds
        let category = {
            let meta = self.metacognition.lock().await;
            meta.thresholds.category_for(composite_error)
        };

        let error = PredictionError {
            delta_satisfaction,
            topic_surprise,
            unexpected_correction,
            unexpected_follow_up,
            composite_error,
            category,
            prediction: prediction.clone(),
            actual: actual.clone(),
        };

        // Record in metacognition
        {
            let mut meta = self.metacognition.lock().await;
            meta.record_prediction(&error);
            if meta.should_evaluate() {
                meta.evaluate_and_adjust();
            }
        }

        // Record in consecutive errors buffer
        {
            let mut errors = self.consecutive_errors.lock().await;
            let buf = errors.entry(actual.agent_id.clone()).or_insert_with(Vec::new);
            buf.push(category);
            if buf.len() > 10 {
                buf.remove(0);
            }
        }

        // Log prediction error to SQLite (async-friendly via spawn_blocking)
        let db_path = self.db_path.clone();
        let agent_id = actual.agent_id.clone();
        let user_id = actual.user_id.clone();
        let ce = composite_error;
        let cat = format!("{category:?}");
        tokio::task::spawn_blocking(move || {
            if let Ok(conn) = Connection::open(&db_path) {
                let _ = conn.execute(
                    "INSERT INTO prediction_log (agent_id, user_id, composite_error, category, timestamp)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![agent_id, user_id, ce, cat, Utc::now().to_rfc3339()],
                );
            }
        });

        debug!(
            agent = %actual.agent_id,
            composite = format!("{composite_error:.3}"),
            category = ?category,
            "Prediction error calculated"
        );

        error
    }

    /// Update the user model after a conversation and optionally persist.
    pub async fn update_model(&self, metrics: &ConversationMetrics) {
        let key = (metrics.user_id.clone(), metrics.agent_id.clone());

        {
            let mut models = self.models.lock().await;
            let model = models
                .entry(key.clone())
                .or_insert_with(|| UserModel::new(metrics.user_id.clone(), metrics.agent_id.clone()));
            model.update_from_metrics(metrics);
        }

        // Debounced persistence
        let should_save = {
            let mut counts = self.update_counts.lock().await;
            let count = counts.entry(key.clone()).or_insert(0);
            *count += 1;
            if *count >= self.save_interval {
                *count = 0;
                true
            } else {
                false
            }
        };

        if should_save {
            self.save_model(&key.0, &key.1).await;
        }
    }

    /// Count trailing Significant+ errors for an agent.
    pub async fn consecutive_significant_count(&self, agent_id: &str) -> usize {
        let errors = self.consecutive_errors.lock().await;
        if let Some(buf) = errors.get(agent_id) {
            buf.iter()
                .rev()
                .take_while(|&&c| matches!(c, ErrorCategory::Significant | ErrorCategory::Critical))
                .count()
        } else {
            0
        }
    }

    /// Persist a single model to SQLite.
    async fn save_model(&self, user_id: &str, agent_id: &str) {
        let model = {
            let models = self.models.lock().await;
            models.get(&(user_id.to_string(), agent_id.to_string())).cloned()
        };

        if let Some(model) = model {
            let db_path = self.db_path.clone();
            let uid = user_id.to_string();
            let aid = agent_id.to_string();
            tokio::task::spawn_blocking(move || {
                if let Ok(conn) = Connection::open(&db_path) {
                    let json = serde_json::to_string(&model).unwrap_or_default();
                    let _ = conn.execute(
                        "INSERT OR REPLACE INTO user_models (user_id, agent_id, model_json, total_conversations, last_updated)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![uid, aid, json, model.total_conversations, model.last_updated.to_rfc3339()],
                    );
                }
            });
        }
    }

    /// Persist metacognition state to disk.
    pub async fn persist_metacognition(&self, path: &Path) {
        let meta = self.metacognition.lock().await;
        meta.persist(path);
    }

    /// Flush all dirty models to disk (call on shutdown).
    pub async fn flush_all(&self) {
        let models = self.models.lock().await;
        let keys: Vec<(String, String)> = models.keys().cloned().collect();
        drop(models);

        for (user_id, agent_id) in keys {
            self.save_model(&user_id, &agent_id).await;
        }
        info!("Flushed all user models to disk");
    }
}
