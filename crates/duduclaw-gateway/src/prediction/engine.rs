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
use super::router::{ConsistencyTracker, ExplorationState};
use super::user_model::UserModel;

use duduclaw_inference::embedding::EmbeddingProvider;

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

use super::outcome::{ConversationOutcome, SatisfactionSignal};

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
    /// Task was detected as incomplete/failed (Phase 1 GVU²).
    #[serde(default)]
    pub task_completion_failure: bool,
    /// Weighted combination of all error signals.
    pub composite_error: f64,
    /// Classification based on composite_error magnitude.
    pub category: ErrorCategory,
    /// The original prediction.
    pub prediction: Prediction,
    /// The actual conversation metrics.
    pub actual: ConversationMetrics,
}

impl PredictionError {
    /// Apply a ConversationOutcome to adjust the composite error (Phase 1 GVU²).
    ///
    /// Call this after `calculate_error()` to incorporate task completion signals.
    /// Adjusts composite_error by adding a weighted task_completion penalty,
    /// then reclassifies the error category.
    pub fn apply_outcome(&mut self, outcome: &ConversationOutcome, metacognition: &super::metacognition::AdaptiveThresholds) {
        if outcome.task_completed == Some(false) || outcome.satisfaction == SatisfactionSignal::Negative {
            self.task_completion_failure = true;

            // Proportional penalty: boost error by 15% of remaining headroom (review #24).
            // Low errors get a larger absolute bump; high errors barely change.
            // E.g., 0.15 → 0.15 + 0.85*0.15 = 0.278 (Negligible→Moderate)
            //        0.85 → 0.85 + 0.15*0.15 = 0.873 (barely changes)
            let headroom = 1.0 - self.composite_error;
            self.composite_error = (self.composite_error + headroom * 0.15).clamp(0.0, 1.0);

            // Reclassify with updated composite
            self.category = metacognition.category_for(self.composite_error);
        }
    }
}

// ---------------------------------------------------------------------------
// DriftBudget — SOUL.md evolution drift constraint
// ---------------------------------------------------------------------------

/// Constrains how far SOUL.md can drift from its original baseline.
///
/// Based on the Drift Bounds Theorem from "Agent Behavioral Contracts"
/// (arXiv:2602.22302): contracts with recovery rate γ > α (drift rate)
/// bound drift to D* = α/γ in expectation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftBudget {
    /// Original SOUL.md content (or behaviors section) for distance calculation.
    pub baseline: String,
    /// Maximum allowed Levenshtein distance as a ratio of baseline length.
    pub max_drift_ratio: f64,
}

impl DriftBudget {
    pub fn new(baseline: String, max_drift_ratio: f64) -> Self {
        Self { baseline, max_drift_ratio }
    }

    /// Check if proposed content is within the drift budget.
    pub fn can_apply(&self, proposed: &str) -> bool {
        if self.baseline.is_empty() {
            return true;
        }
        let distance = Self::bigram_jaccard_distance(&self.baseline, proposed);
        distance <= self.max_drift_ratio
    }

    /// Remaining drift budget (0.0 = exhausted, 1.0 = full budget).
    pub fn remaining(&self, current: &str) -> f64 {
        if self.baseline.is_empty() {
            return 1.0;
        }
        let used = Self::bigram_jaccard_distance(&self.baseline, current);
        (self.max_drift_ratio - used).max(0.0) / self.max_drift_ratio
    }

    /// Character-bigram Jaccard distance as a proxy for content drift.
    ///
    /// **Limitation**: This is order-insensitive — reordering paragraphs yields
    /// distance ≈ 0.0 even though the structural meaning may change.
    /// For order-sensitive drift detection, consider greedy string tiling.
    /// Current use is acceptable because GVU proposals are appended, not reordered.
    fn bigram_jaccard_distance(a: &str, b: &str) -> f64 {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let max_len = a_chars.len().max(b_chars.len());
        if max_len == 0 {
            return 0.0;
        }

        // Use char-bigram Jaccard distance as proxy for edit distance ratio
        fn bigrams(chars: &[char]) -> HashMap<(char, char), u32> {
            let mut freq = HashMap::new();
            for w in chars.windows(2) {
                *freq.entry((w[0], w[1])).or_insert(0) += 1;
            }
            freq
        }

        let a_bg = bigrams(&a_chars);
        let b_bg = bigrams(&b_chars);
        let all_keys: std::collections::HashSet<&(char, char)> =
            a_bg.keys().chain(b_bg.keys()).collect();

        let mut intersection = 0u32;
        let mut union = 0u32;
        for key in all_keys {
            let ca = a_bg.get(key).copied().unwrap_or(0);
            let cb = b_bg.get(key).copied().unwrap_or(0);
            intersection += ca.min(cb);
            union += ca.max(cb);
        }

        if union == 0 { 0.0 } else { 1.0 - (intersection as f64 / union as f64) }
    }
}

// ---------------------------------------------------------------------------
// EvolutionHealthMonitor — mode collapse / oscillation detection
// ---------------------------------------------------------------------------

/// Monitors the health of the GVU evolution loop by tracking improvements.
///
/// Detects two pathological states:
/// - **Stalled**: Expected improvement approaches zero → mode collapse.
/// - **Oscillating**: High variance in improvements → random walk.
///
/// Based on the Variance Inequality from "Self-Improving AI Agents through
/// Self-Play" (arXiv:2512.02731).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvolutionHealth {
    Healthy,
    Stalled,
    Oscillating,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionHealthMonitor {
    /// Recent GVU improvement scores (positive = improved, negative = regressed).
    improvements: std::collections::VecDeque<f64>,
    /// Maximum window size.
    window_size: usize,
}

impl Default for EvolutionHealthMonitor {
    fn default() -> Self {
        Self {
            improvements: std::collections::VecDeque::new(),
            window_size: 20,
        }
    }
}

impl EvolutionHealthMonitor {
    /// Record a GVU cycle's improvement score.
    pub fn record(&mut self, improvement: f64) {
        self.improvements.push_back(improvement);
        while self.improvements.len() > self.window_size {
            self.improvements.pop_front();
        }
    }

    /// Assess current evolution health.
    pub fn health(&self) -> EvolutionHealth {
        if self.improvements.len() < 5 {
            return EvolutionHealth::Healthy; // Not enough data
        }

        let mean = self.improvements.iter().sum::<f64>() / self.improvements.len() as f64;
        let variance = self.improvements.iter()
            .map(|x| (x - mean).powi(2))
            .sum::<f64>() / self.improvements.len() as f64;

        if mean.abs() < 0.01 && variance < 0.001 {
            EvolutionHealth::Stalled
        } else if variance > mean.abs() * 2.0 && variance > 0.01 {
            // Absolute floor (0.01) prevents false Oscillating when mean ≈ 0 (audit #17)
            EvolutionHealth::Oscillating
        } else {
            EvolutionHealth::Healthy
        }
    }
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
    consecutive_errors: Arc<Mutex<HashMap<String, std::collections::VecDeque<ErrorCategory>>>>,
    /// Self-calibrating metacognition system.
    pub metacognition: Arc<Mutex<MetaCognition>>,
    /// How often to save models (every N updates).
    save_interval: u64,
    /// Updates since last save, per model key.
    update_counts: Arc<Mutex<HashMap<(String, String), u64>>>,
    /// SOUL.md drift budget per agent.
    pub drift_budgets: Arc<Mutex<HashMap<String, DriftBudget>>>,
    /// GVU evolution health monitor per agent.
    pub health_monitors: Arc<Mutex<HashMap<String, EvolutionHealthMonitor>>>,
    /// Exploration state for epsilon-floor routing.
    pub exploration: Arc<Mutex<ExplorationState>>,
    /// Consistency tracker for anti-sycophancy.
    pub consistency: Arc<Mutex<ConsistencyTracker>>,
    /// Optional embedding provider for semantic topic similarity.
    /// When available, topic_surprise uses cosine distance instead of keyword overlap.
    /// When None, falls back to vocabulary_novelty → keyword matching.
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    /// Maximum embedding history per user-agent pair.
    max_embedding_history: usize,
}

impl PredictionEngine {
    /// Create a new prediction engine, initializing SQLite tables and loading cached models.
    pub fn new(
        db_path: PathBuf,
        meta_path: Option<PathBuf>,
    ) -> Self {
        Self::new_with_embedding(db_path, meta_path, None, 100)
    }

    /// Create with an optional embedding provider.
    pub fn new_with_embedding(
        db_path: PathBuf,
        meta_path: Option<PathBuf>,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
        max_embedding_history: usize,
    ) -> Self {
        // Initialize database tables
        if let Ok(conn) = Connection::open(&db_path) {
            if let Err(e) = Self::init_tables(&conn) {
                warn!("Failed to init prediction tables: {e}");
            }
        }

        // Load metacognition state, then BUG-4 fix: rehydrate counters
        // from prediction.db so a restart can't make total_predictions
        // permanently lag behind the SQLite log (which would prevent
        // evaluate_and_adjust from ever firing).
        let mut metacognition = meta_path
            .as_ref()
            .and_then(|p| MetaCognition::load(p))
            .unwrap_or_default();
        if let Err(e) = metacognition.rehydrate_from_db(&db_path) {
            warn!("MetaCognition rehydrate skipped: {e}");
        } else {
            metacognition.force_evaluation_if_overdue();
        }

        let engine = Self {
            models: Arc::new(Mutex::new(HashMap::new())),
            db_path,
            consecutive_errors: Arc::new(Mutex::new(HashMap::new())),
            metacognition: Arc::new(Mutex::new(metacognition)),
            save_interval: 5,
            update_counts: Arc::new(Mutex::new(HashMap::new())),
            drift_budgets: Arc::new(Mutex::new(HashMap::new())),
            health_monitors: Arc::new(Mutex::new(HashMap::new())),
            exploration: Arc::new(Mutex::new(ExplorationState::default())),
            consistency: Arc::new(Mutex::new(ConsistencyTracker::new(50))),
            embedding_provider,
            max_embedding_history,
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
                ON prediction_log(agent_id);

            CREATE TABLE IF NOT EXISTS evolution_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                composite_error REAL,
                error_category TEXT,
                trigger_context TEXT,
                soul_diff TEXT,
                version_id TEXT,
                rollback_reason TEXT,
                ext_validation_type TEXT,
                ext_validation_value REAL,
                ext_validation_timestamp TEXT,
                timestamp TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_evolution_agent_ts
                ON evolution_events(agent_id, timestamp);
            CREATE INDEX IF NOT EXISTS idx_evolution_type
                ON evolution_events(event_type);"
        ).map_err(|e| e.to_string())
    }

    /// Log a structured evolution event to SQLite (Sutskever Day 1 principle).
    ///
    /// Non-blocking: spawns a blocking task for the SQLite write.
    pub fn log_evolution_event(
        &self,
        event_type: &str,
        agent_id: &str,
        composite_error: Option<f64>,
        error_category: Option<&str>,
        trigger_context: Option<&str>,
        version_id: Option<&str>,
        rollback_reason: Option<&str>,
    ) {
        let db_path = self.db_path.clone();
        let event_id = uuid::Uuid::new_v4().to_string();
        let agent = agent_id.to_string();
        let etype = event_type.to_string();
        let ce = composite_error;
        let cat = error_category.map(String::from);
        let ctx = trigger_context.map(|s| s.chars().take(500).collect::<String>());
        let vid = version_id.map(String::from);
        let rr = rollback_reason.map(String::from);
        let ts = Utc::now().to_rfc3339();

        tokio::task::spawn_blocking(move || {
            if let Ok(conn) = Connection::open(&db_path) {
                let _ = conn.execute(
                    "INSERT INTO evolution_events
                     (event_id, agent_id, event_type, composite_error, error_category,
                      trigger_context, version_id, rollback_reason, timestamp)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![event_id, agent, etype, ce, cat, ctx, vid, rr, ts],
                );
            }
        });
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

    /// Calculate the prediction error and optionally return the computed embedding.
    ///
    /// The returned embedding (if any) should be passed to `update_model_with_embedding`
    /// to avoid redundant embedding computation.
    pub async fn calculate_error(
        &self,
        prediction: &Prediction,
        actual: &ConversationMetrics,
    ) -> (PredictionError, Option<Vec<f32>>) {
        // --- Infer actual satisfaction from graded behavioural signals ---
        let mut inferred_satisfaction: f64 = 0.7; // neutral baseline

        // Use graded feedback score instead of binary correction count.
        // (EMNLP 2025 "User Feedback in Human-LLM Dialogues")
        //
        // Fallback logic: check if FeedbackDetail was actually populated (has any
        // severity counts), not just whether the score is > 0. A Clarification-only
        // conversation has score 0.1 but should NOT fall back to raw count * 0.3.
        let has_feedback_detail = !actual.feedback_details.severity_counts.is_empty();
        let correction_impact = if has_feedback_detail {
            actual.feedback_details.weighted_correction_score * 0.3
        } else {
            actual.user_corrections as f64 * 0.3 // fallback for old data without FeedbackDetail
        };

        // Counterfactual robustness check (arXiv:2501.09620):
        // If correction was detected but topics didn't change, the user may be
        // clarifying their own intent rather than correcting the agent.
        let counterfactual_discount = if actual.user_corrections > 0 {
            let topics_changed = prediction.expected_topic.as_ref().map_or(true, |expected| {
                !actual.extracted_topics.iter().any(|t| t == expected)
            });
            if topics_changed { 1.0 } else { 0.5 } // Discount unreliable corrections
        } else {
            1.0
        };

        inferred_satisfaction -= correction_impact * counterfactual_discount;
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

        // Topic surprise: 3-tier fallback
        // Tier 1: Embedding cosine similarity (most accurate, requires model)
        // Tier 2: Vocabulary novelty (HashSet-based change detection)
        // Tier 3: Keyword overlap (original, least accurate for CJK)
        //
        // IMPORTANT: Embed OUTSIDE the models lock to avoid deadlock.
        // The embed() call takes ~5ms and must not hold the mutex.
        let current_embedding = if let Some(ref provider) = self.embedding_provider {
            if !actual.user_text.is_empty() {
                provider.embed(&actual.user_text).await.ok()
            } else {
                None
            }
        } else {
            None
        };

        let (topic_surprise, has_embedding) = {
            let models = self.models.lock().await;
            let key = (actual.user_id.clone(), actual.agent_id.clone());
            let model = models.get(&key);

            if let Some(ref emb) = current_embedding {
                // Tier 1: Embedding-based semantic surprise
                let surprise = if let Some(m) = model {
                    Self::compute_embedding_surprise(emb, &m.topic_embeddings)
                } else {
                    0.5 // First conversation — moderate surprise (not 0.0)
                };
                (surprise, true)
            } else if let Some(m) = model {
                // Tier 2: Vocabulary novelty (no embedding model or embed failed)
                let surprise = Self::compute_vocabulary_novelty(&actual.user_text, &m.historical_bigrams);
                (surprise, false)
            } else {
                // Tier 3: Keyword overlap (cold start, no history at all)
                let surprise = Self::keyword_topic_surprise(prediction, actual);
                (surprise, false)
            }
        };

        // Unexpected correction: uses graded score when available, raw count as fallback.
        // Consistent with the has_feedback_detail check above.
        let unexpected_correction = prediction.expected_satisfaction > 0.6 && if has_feedback_detail {
            actual.feedback_details.weighted_correction_score > 0.5
        } else {
            actual.user_corrections > 0
        };

        let unexpected_follow_up = prediction.expected_follow_up_rate < 0.3
            && actual.user_follow_ups > 2;

        // Indirect disagreement signal (from FeedbackDetail)
        let indirect_disagreement_score = actual.feedback_details.severity_counts
            .get("IndirectDisagreement")
            .copied()
            .unwrap_or(0) as f64
            * 0.3; // Each indirect disagreement contributes 0.3

        // Dynamic composite error weights based on embedding availability.
        // With embedding: topic_surprise is semantically meaningful (20% weight).
        // Without embedding: topic_surprise is unreliable for CJK (5% weight).
        let (w_sat, w_topic, w_corr, w_follow, w_indirect) = if has_embedding {
            (0.30, 0.20, 0.25, 0.15, 0.10)
        } else {
            (0.45, 0.05, 0.25, 0.15, 0.10)
        };

        let composite_error = (w_sat * delta_satisfaction.abs()
            + w_topic * topic_surprise
            + w_corr * if unexpected_correction { 1.0 } else { 0.0 }
            + w_follow * if unexpected_follow_up { 1.0 } else { 0.0 }
            + w_indirect * indirect_disagreement_score.min(1.0))
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
            task_completion_failure: false, // Set by caller via apply_outcome()
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
            let buf = errors.entry(actual.agent_id.clone()).or_insert_with(std::collections::VecDeque::new);
            buf.push_back(category);
            while buf.len() > 10 {
                buf.pop_front();
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
            has_embedding = current_embedding.is_some(),
            "Prediction error calculated"
        );

        (error, current_embedding)
    }

    /// Update the user model after a conversation and optionally persist.
    ///
    /// Prefer `update_model_with_embedding` when an embedding was already
    /// computed by `calculate_error` to avoid redundant computation.
    pub async fn update_model(&self, metrics: &ConversationMetrics) {
        self.update_model_with_embedding(metrics, None).await;
    }

    /// Update the user model with a pre-computed embedding.
    ///
    /// Pass the embedding returned by `calculate_error` to avoid calling
    /// `embed()` twice on the same text.
    pub async fn update_model_with_embedding(
        &self,
        metrics: &ConversationMetrics,
        embedding: Option<Vec<f32>>,
    ) {
        let key = (metrics.user_id.clone(), metrics.agent_id.clone());

        {
            let mut models = self.models.lock().await;
            let model = models
                .entry(key.clone())
                .or_insert_with(|| UserModel::new(metrics.user_id.clone(), metrics.agent_id.clone()));
            model.update_from_metrics(metrics);
            if let Some(emb) = embedding {
                model.update_embedding(emb, self.max_embedding_history);
            }
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

    // ── Embedding-based topic surprise (Hardening 2025-Q2) ─────

    /// Compute topic surprise using time-weighted cosine similarity
    /// against historical conversation embeddings.
    ///
    /// Surprise = 1.0 - weighted_avg_similarity. High surprise means
    /// the current conversation is semantically distant from history.
    ///
    /// Uses exponential decay with 7-day half-life to prioritize recent topics.
    fn compute_embedding_surprise(
        current: &[f32],
        history: &std::collections::VecDeque<(Vec<f32>, f64)>,
    ) -> f64 {
        if history.is_empty() {
            // No history = first conversation with this agent.
            // Return moderate surprise (0.5) rather than 0.0.
            // 0.0 would mean "perfectly predicted" which is wrong for a first encounter.
            return 0.5;
        }

        let now = chrono::Utc::now().timestamp() as f64;
        let decay_half_life = 7.0 * 24.0 * 3600.0; // 7 days in seconds

        let mut weighted_sim_sum = 0.0_f64;
        let mut weight_sum = 0.0_f64;

        for (hist_embedding, timestamp) in history {
            let age_secs = (now - timestamp).max(0.0);
            let weight = (-age_secs * 0.693 / decay_half_life).exp(); // ln(2) ≈ 0.693

            let sim = duduclaw_memory::embedding::cosine_similarity(current, hist_embedding) as f64;
            weighted_sim_sum += sim * weight;
            weight_sum += weight;
        }

        if weight_sum < 1e-10 {
            return 0.0;
        }

        let avg_similarity = weighted_sim_sum / weight_sum;
        // Surprise = 1 - similarity, clamped to [0, 1]
        (1.0 - avg_similarity).clamp(0.0, 1.0)
    }

    /// Compute vocabulary novelty using character-bigram set difference.
    ///
    /// novelty = |current_bigrams - historical_bigrams| / |current_bigrams|
    /// High novelty means the user is discussing something entirely new.
    /// This is the fallback when no embedding model is available.
    fn compute_vocabulary_novelty(
        user_text: &str,
        historical_bigrams: &std::collections::HashSet<String>,
    ) -> f64 {
        let chars: Vec<char> = user_text.chars().filter(|c| !c.is_whitespace()).collect();
        let current_bigrams: std::collections::HashSet<String> = chars
            .windows(2)
            .map(|w| w.iter().collect::<String>())
            .collect();

        if current_bigrams.is_empty() {
            return 0.0;
        }

        let novel = current_bigrams.difference(historical_bigrams).count();
        novel as f64 / current_bigrams.len() as f64
    }

    /// Keyword-based topic surprise (original Tier 3 fallback).
    ///
    /// Used only during cold start when no embedding or bigram history exists.
    fn keyword_topic_surprise(prediction: &Prediction, actual: &ConversationMetrics) -> f64 {
        if let Some(ref expected) = prediction.expected_topic {
            if actual.extracted_topics.iter().any(|t| t == expected) {
                0.0
            } else {
                let best_overlap = actual.extracted_topics.iter().map(|t| {
                    let expected_chars: std::collections::HashSet<char> = expected.chars().collect();
                    let topic_chars: std::collections::HashSet<char> = t.chars().collect();
                    let inter = expected_chars.intersection(&topic_chars).count() as f64;
                    let union = expected_chars.union(&topic_chars).count().max(1) as f64;
                    inter / union
                }).fold(0.0_f64, f64::max);
                (1.0 - best_overlap) * 0.7
            }
        } else {
            0.0
        }
    }
}
