# Evolution Engine v2 — Predictive Processing + GVU Self-Play + Cognitive Memory

> Combined architecture: **I (Predictive Processing)** as event-driven backbone + **H (GVU Loop)** as evolution quality core + **G (Cognitive Memory)** as structured memory layer.
>
> Theoretical foundations:
> - CoALA (arXiv 2309.02427), Generative Agents (arXiv 2304.03442)
> - GVU Self-Play (arXiv 2512.02731), OPRO (arXiv 2309.03409)
> - TextGrad (arXiv 2406.07496), Reflexion (arXiv 2303.11366)
> - Active Inference / Free Energy Principle (Friston)
> - Dual Process Theory (Kahneman), Metacognitive Learning (ICML 2025)

---

## Legend

- `[NEW]` = new file
- `[MOD]` = modify existing file
- `[DEL]` = delete file
- `[MIG]` = SQLite migration
- `[TST]` = test file
- `[CFG]` = configuration change
- Lines reference current codebase snapshot (v0.6.6)

---

## Phase 1: Predictive Processing Engine (Event-Driven Backbone) ✅ IMPLEMENTED

> Goal: Replace fixed heartbeat scheduling with prediction-error-driven evolution triggers.
> 90% of conversations should complete with zero LLM cost (System 1 path).
>
> **Status: COMPLETE** (2026-03-27) — 27 tests passing, all crates build clean.
>
> ### Implementation Summary
>
> | File | Type | Lines | Description |
> |------|------|:-----:|-------------|
> | `prediction/mod.rs` | NEW | 17 | Module declarations |
> | `prediction/user_model.rs` | NEW | 195 | RunningStats (Welford), LanguageStats, UserModel |
> | `prediction/metrics.rs` | NEW | 210 | ConversationMetrics extraction, keyword/language detection |
> | `prediction/engine.rs` | NEW | 290 | PredictionEngine, Prediction, PredictionError, ErrorCategory |
> | `prediction/router.rs` | NEW | 120 | DualProcessRouter, EvolutionAction dispatch |
> | `prediction/metacognition.rs` | NEW | 210 | AdaptiveThresholds, MetaCognition self-calibration |
> | `prediction/tests.rs` | TST | 280 | 27 unit tests across all modules |
> | `types.rs` | MOD | +18 | prediction_driven, gvu_enabled, cognitive_memory, max_silence_hours |
> | `channel_reply.rs` | MOD | +55 | Prediction engine integration in build_reply_with_session() |
> | `heartbeat.rs` | MOD | +30 | prediction_driven mode, silence checker, last_evolution_trigger |
> | `evolution.rs` | MOD | +4 | Default values for new EvolutionConfig fields |
> | `lib.rs` | MOD | +1 | pub mod prediction |
> | **Total** | | **~1,430** | |

### 1.1 UserModel — Per-User Statistical Model

#### 1.1.1 Define core data structures `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/prediction/mod.rs`
  - [ ] Declare submodule exports: `engine`, `error`, `user_model`, `metacognition`
- [ ] Create `crates/duduclaw-gateway/src/prediction/user_model.rs`
  - [ ] Define `RunningStats` struct (online mean + variance via Welford's algorithm)
    - [ ] `count: u64`
    - [ ] `mean: f64`
    - [ ] `m2: f64` (for variance calculation)
    - [ ] Implement `push(value: f64)` method
    - [ ] Implement `mean() -> f64` method
    - [ ] Implement `variance() -> f64` method
    - [ ] Implement `std_dev() -> f64` method
    - [ ] Implement `sample_count() -> u64` method
    - [ ] Implement `Default` trait
    - [ ] Implement `Serialize` / `Deserialize` for persistence
  - [ ] Define `LanguageStats` struct
    - [ ] `primary_language: String` (e.g., "zh-TW")
    - [ ] `language_distribution: HashMap<String, f64>`
    - [ ] Implement `update(detected_lang: &str)` method
  - [ ] Define `UserModel` struct
    - [ ] `user_id: String`
    - [ ] `agent_id: String`
    - [ ] `preferred_response_length: RunningStats`
    - [ ] `avg_satisfaction: RunningStats` (derived from feedback signals)
    - [ ] `topic_distribution: HashMap<String, f64>` (keyword frequency TF-IDF)
    - [ ] `active_hours: [f64; 24]` (per-hour activity probability)
    - [ ] `correction_rate: RunningStats` (user correction frequency)
    - [ ] `follow_up_rate: RunningStats` (user follow-up question rate)
    - [ ] `avg_response_time_preference: RunningStats` (ms)
    - [ ] `language_preference: LanguageStats`
    - [ ] `total_conversations: u64`
    - [ ] `last_updated: DateTime<Utc>`
    - [ ] Implement `Default` trait (cold-start defaults)
    - [ ] Implement `Serialize` / `Deserialize`

#### 1.1.2 Implement UserModel update logic `[NEW]`

- [ ] In `user_model.rs`:
  - [ ] Implement `update_from_conversation(&mut self, metrics: &ConversationMetrics)`
    - [ ] Update `preferred_response_length` from assistant message lengths
    - [ ] Update `follow_up_rate` from consecutive user messages
    - [ ] Update `topic_distribution` from extracted keywords
    - [ ] Update `active_hours` based on message timestamps
    - [ ] Update `avg_response_time_preference` from response durations
    - [ ] Update `language_preference` from detected language
    - [ ] Increment `total_conversations`
    - [ ] Set `last_updated` to now
  - [ ] Implement `update_from_feedback(&mut self, feedback: &FeedbackSignal)`
    - [ ] Map positive/negative/correction to satisfaction score
    - [ ] Update `avg_satisfaction`
    - [ ] If correction: update `correction_rate`

#### 1.1.3 UserModel persistence (SQLite) `[MIG]`

- [ ] Create migration in `crates/duduclaw-gateway/src/prediction/schema.sql`
  ```sql
  CREATE TABLE IF NOT EXISTS user_models (
      user_id TEXT NOT NULL,
      agent_id TEXT NOT NULL,
      model_json TEXT NOT NULL,        -- serialized UserModel
      total_conversations INTEGER DEFAULT 0,
      last_updated TEXT NOT NULL,
      PRIMARY KEY (user_id, agent_id)
  );
  CREATE INDEX IF NOT EXISTS idx_user_models_agent ON user_models(agent_id);
  ```
- [ ] Add `init_prediction_tables()` function
- [ ] Call `init_prediction_tables()` from `SessionManager::new()` (session.rs:47)
- [ ] Implement `load_user_model(user_id, agent_id) -> Option<UserModel>`
- [ ] Implement `save_user_model(model: &UserModel)`
- [ ] Add debounced save (save after every 5 conversations, not every message)

### 1.2 ConversationMetrics — Per-Conversation Signal Extraction

#### 1.2.1 Define metrics struct `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/prediction/metrics.rs`
  - [ ] Define `ConversationMetrics` struct
    - [ ] `session_id: String`
    - [ ] `user_id: String`
    - [ ] `agent_id: String`
    - [ ] `message_count: u32`
    - [ ] `user_message_count: u32`
    - [ ] `assistant_message_count: u32`
    - [ ] `avg_assistant_response_length: f64` (chars)
    - [ ] `total_tokens: u32`
    - [ ] `response_time_ms: u64`
    - [ ] `user_follow_ups: u32` (consecutive user messages after assistant)
    - [ ] `user_corrections: u32` (detected correction patterns)
    - [ ] `detected_language: String`
    - [ ] `extracted_topics: Vec<String>` (top-5 keywords by TF-IDF)
    - [ ] `ended_naturally: bool` (vs timeout/error)
    - [ ] `feedback: Option<FeedbackSignal>` (if submitted during/after conversation)
    - [ ] `timestamp: DateTime<Utc>`

#### 1.2.2 Implement metrics extraction `[NEW]`

- [ ] In `metrics.rs`:
  - [ ] Implement `extract_from_session(session: &Session, messages: &[SessionMessage]) -> ConversationMetrics`
    - [ ] Count messages by role
    - [ ] Calculate avg assistant response length
    - [ ] Count follow-up patterns (user→assistant→user where second user msg < 30 chars or contains "?")
    - [ ] Detect correction patterns (user message contains "not", "wrong", "no," "不是", "錯了", "不對")
    - [ ] Extract top-5 keywords using simple TF-IDF (CJK segmentation: character bigrams)
    - [ ] Detect primary language (CJK char ratio threshold)
  - [ ] Implement simple keyword extraction function
    - [ ] `extract_keywords(text: &str, top_k: usize) -> Vec<String>`
    - [ ] Handle CJK: use character bigrams as terms
    - [ ] Handle ASCII: split on whitespace, filter stopwords
    - [ ] Return top-k by frequency

### 1.3 PredictionEngine — Core Prediction + Error Calculation

#### 1.3.1 Define Prediction and PredictionError structs `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/prediction/engine.rs`
  - [ ] Define `Prediction` struct
    - [ ] `expected_satisfaction: f64` (0.0 - 1.0)
    - [ ] `expected_follow_up_rate: f64`
    - [ ] `expected_topic: Option<String>`
    - [ ] `confidence: f64` (0.0 - 1.0, based on sample count)
    - [ ] `timestamp: DateTime<Utc>`
  - [ ] Define `PredictionError` struct
    - [ ] `delta_satisfaction: f64`
    - [ ] `topic_surprise: f64` (0.0 - 1.0, keyword overlap distance)
    - [ ] `unexpected_correction: bool`
    - [ ] `unexpected_follow_up: bool`
    - [ ] `composite_error: f64` (weighted combination)
    - [ ] `category: ErrorCategory`
    - [ ] `prediction: Prediction` (the original prediction)
    - [ ] `actual: ConversationMetrics` (the actual outcome)
  - [ ] Define `ErrorCategory` enum
    - [ ] `Negligible` (composite_error < 0.2)
    - [ ] `Moderate` (0.2 <= composite_error < 0.5)
    - [ ] `Significant` (0.5 <= composite_error < 0.8)
    - [ ] `Critical` (composite_error >= 0.8)

#### 1.3.2 Implement PredictionEngine `[NEW]`

- [ ] In `engine.rs`:
  - [ ] Define `PredictionEngine` struct
    - [ ] `models: Arc<RwLock<HashMap<(String, String), UserModel>>>` (user_id, agent_id → model)
    - [ ] `db_path: PathBuf`
    - [ ] `consecutive_errors: HashMap<String, Vec<ErrorCategory>>` (agent_id → recent errors)
  - [ ] Implement `PredictionEngine::new(db_path: PathBuf) -> Self`
    - [ ] Initialize SQLite tables
    - [ ] Load existing models into memory cache
  - [ ] Implement `predict(&self, user_id: &str, agent_id: &str, message: &str) -> Prediction`
    - [ ] Load or create UserModel
    - [ ] Calculate expected_satisfaction from model.avg_satisfaction.mean()
    - [ ] Calculate expected_follow_up_rate from model.follow_up_rate.mean()
    - [ ] Calculate expected_topic from model.topic_distribution
    - [ ] Calculate confidence = min(model.total_conversations, 50) / 50.0
    - [ ] Return Prediction
  - [ ] Implement `calculate_error(&self, prediction: &Prediction, actual: &ConversationMetrics) -> PredictionError`
    - [ ] delta_satisfaction = predicted - inferred_actual_satisfaction
    - [ ] Infer actual satisfaction: 1.0 base, -0.3 per correction, -0.1 per follow-up, +0.2 for positive feedback, -0.4 for negative feedback
    - [ ] topic_surprise = 1.0 - keyword_overlap(predicted_topic, actual.extracted_topics)
    - [ ] unexpected_correction = !predicted_high_correction && actual.user_corrections > 0
    - [ ] unexpected_follow_up = predicted_low_follow_up && actual.user_follow_ups > 2
    - [ ] composite_error = 0.4 * |delta_satisfaction| + 0.2 * topic_surprise + 0.2 * unexpected_correction as f64 + 0.2 * unexpected_follow_up as f64
    - [ ] Determine ErrorCategory from composite_error
  - [ ] Implement `record_error(&mut self, agent_id: &str, error: &PredictionError)`
    - [ ] Push to consecutive_errors ring buffer (keep last 10)
  - [ ] Implement `consecutive_significant_count(&self, agent_id: &str) -> usize`
    - [ ] Count trailing Significant+ errors in buffer

### 1.4 Dual-Process Router — System 1 / System 2 Dispatch

#### 1.4.1 Define DualProcessRouter `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/prediction/router.rs`
  - [ ] Define `EvolutionAction` enum
    - [ ] `None` — no action (System 1 only)
    - [ ] `StoreEpisodic { content: String, importance: f64 }` — store memory only
    - [ ] `TriggerReflection { context: String }` — invoke LLM reflection
    - [ ] `TriggerEmergencyEvolution { error: PredictionError }` — immediate GVU loop
  - [ ] Implement `route(error: &PredictionError, consecutive_significant: usize) -> EvolutionAction`
    - [ ] Negligible → `EvolutionAction::None`
    - [ ] Moderate → `EvolutionAction::StoreEpisodic`
    - [ ] Significant → `EvolutionAction::TriggerReflection`
    - [ ] Significant + consecutive >= 3 → `EvolutionAction::TriggerEmergencyEvolution`
    - [ ] Critical → `EvolutionAction::TriggerEmergencyEvolution`

#### 1.4.2 Integrate into channel_reply.rs `[MOD]`

- [ ] Modify `build_reply_with_session()` (channel_reply.rs:75)
  - [ ] Before calling Claude: invoke `prediction_engine.predict()`, store prediction in local variable
  - [ ] After receiving Claude response: extract `ConversationMetrics`
  - [ ] Calculate `PredictionError`
  - [ ] Update `UserModel` via `prediction_engine.update_model()`
  - [ ] Route through `DualProcessRouter::route()`
  - [ ] Dispatch resulting `EvolutionAction`:
    - [ ] `None` → continue (zero overhead)
    - [ ] `StoreEpisodic` → write to memory table (existing `SqliteMemoryEngine::store()`)
    - [ ] `TriggerReflection` → spawn tokio task calling existing `run_meso()` (evolution.rs:42)
    - [ ] `TriggerEmergencyEvolution` → spawn tokio task for GVU loop (Phase 2 hook, initially fallback to `run_macro()`)
- [ ] Add `PredictionEngine` to `ReplyContext` struct (channel_reply.rs:23)
  - [ ] Add field `prediction_engine: Arc<PredictionEngine>`
  - [ ] Update `ReplyContext::new()` to accept prediction engine

### 1.5 MetaCognition — Self-Calibrating Thresholds

#### 1.5.1 Define MetaCognition `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/prediction/metacognition.rs`
  - [ ] Define `AdaptiveThresholds` struct
    - [ ] `negligible_upper: f64` (default 0.2)
    - [ ] `moderate_upper: f64` (default 0.5)
    - [ ] `significant_upper: f64` (default 0.8)
    - [ ] Implement `category_for(composite_error: f64) -> ErrorCategory`
    - [ ] Implement `Serialize` / `Deserialize`
  - [ ] Define `LayerEffectiveness` struct
    - [ ] `triggers: u64` (how many times this layer triggered)
    - [ ] `improvements: u64` (how many times triggering led to improvement)
    - [ ] `improvement_rate() -> f64`
  - [ ] Define `MetaCognition` struct
    - [ ] `thresholds: AdaptiveThresholds`
    - [ ] `prediction_accuracy: RunningStats`
    - [ ] `layer_stats: HashMap<ErrorCategory, LayerEffectiveness>`
    - [ ] `evaluation_interval: u64` (default 100 predictions)
    - [ ] `predictions_since_last_eval: u64`

#### 1.5.2 Implement metacognitive evaluation `[NEW]`

- [ ] In `metacognition.rs`:
  - [ ] Implement `MetaCognition::new() -> Self` with default thresholds
  - [ ] Implement `record_prediction(&mut self, error: &PredictionError)`
    - [ ] Update prediction_accuracy with composite_error
    - [ ] Increment predictions_since_last_eval
    - [ ] Increment layer_stats[error.category].triggers
  - [ ] Implement `record_outcome(&mut self, category: ErrorCategory, improved: bool)`
    - [ ] If improved: increment layer_stats[category].improvements
  - [ ] Implement `should_evaluate(&self) -> bool`
    - [ ] Return predictions_since_last_eval >= evaluation_interval
  - [ ] Implement `evaluate_and_adjust(&mut self)`
    - [ ] If Significant effectiveness < 0.3 → raise significant_upper by 0.05 (less sensitive)
    - [ ] If Significant effectiveness > 0.7 → lower significant_upper by 0.03 (more sensitive)
    - [ ] If Critical proportion > 20% of all triggers → lower moderate_upper by 0.05
    - [ ] Clamp all thresholds to [0.1, 0.95]
    - [ ] Reset predictions_since_last_eval to 0
    - [ ] Log threshold adjustments at INFO level
  - [ ] Implement `persist(&self, path: &Path)` — save thresholds to JSON file
  - [ ] Implement `load(path: &Path) -> Option<Self>` — load from JSON file

#### 1.5.3 Wire MetaCognition into PredictionEngine `[MOD]`

- [ ] Add `metacognition: MetaCognition` field to `PredictionEngine`
- [ ] After every `calculate_error()`: call `metacognition.record_prediction()`
- [ ] Use `metacognition.thresholds.category_for()` instead of hardcoded thresholds
- [ ] After every evolution outcome (confirm/rollback in Phase 2): call `metacognition.record_outcome()`
- [ ] After every `record_prediction()`: check `should_evaluate()` and call `evaluate_and_adjust()`
- [ ] On PredictionEngine drop/shutdown: call `metacognition.persist()`

### 1.6 Replace Heartbeat-Driven Evolution Triggers `[MOD]`

#### 1.6.1 Modify HeartbeatScheduler `[MOD]`

- [ ] Modify `HeartbeatScheduler` (heartbeat.rs:144)
  - [ ] Add `prediction_driven: bool` flag to `HeartbeatConfig` (types.rs:63)
  - [ ] When `prediction_driven == true`:
    - [ ] Skip meso/macro timer-based triggers in `execute_heartbeat()` (heartbeat.rs:324)
    - [ ] Keep heartbeat alive for: IPC bus polling, cron task execution, silence checker
  - [ ] Implement silence checker in heartbeat loop:
    - [ ] Track `last_evolution_trigger: DateTime<Utc>` per agent
    - [ ] If `now - last_evolution_trigger > max_silence_hours` (default 12h):
      - [ ] Force-inject a `Moderate` level signal to trigger at least a Meso reflection
      - [ ] Log at WARN level: "Silence breaker triggered for agent {id}"

#### 1.6.2 Update EvolutionConfig `[MOD]`

- [ ] Modify `EvolutionConfig` (types.rs:94)
  - [ ] Add `prediction_driven: bool` field (default false for backward compat)
  - [ ] Add `adaptive_thresholds: bool` field (default true when prediction_driven)
  - [ ] Add `max_silence_hours: f64` field (default 12.0)
- [ ] Update agent.toml parsing to support new fields
- [ ] Update TOML serialization in `handle_create_agent()` (mcp.rs:837)

#### 1.6.3 Backward compatibility `[MOD]`

- [ ] When `prediction_driven == false` (default): behavior unchanged, heartbeat drives evolution
- [ ] When `prediction_driven == true`: prediction errors drive evolution, heartbeat only for bus polling + silence
- [ ] Add `duduclaw doctor` check: warn if `prediction_driven` is true but no conversations have occurred (cold start)

### 1.7 Phase 1 Tests

#### 1.7.1 Unit tests `[TST]`

- [ ] Create `crates/duduclaw-gateway/src/prediction/tests/mod.rs`
- [ ] Test `RunningStats`:
  - [ ] Empty stats return 0.0 for mean and variance
  - [ ] Single value: mean equals value, variance is 0
  - [ ] Known sequence: verify mean and std_dev against hand-calculated values
  - [ ] Large N: verify numerical stability (Welford's should not drift)
- [ ] Test `UserModel`:
  - [ ] Cold start defaults produce valid predictions
  - [ ] `update_from_conversation()` moves stats in expected direction
  - [ ] `update_from_feedback()`: positive increases satisfaction, negative decreases
  - [ ] Correction signals increase correction_rate
- [ ] Test `ConversationMetrics::extract_from_session()`:
  - [ ] Correct message counts by role
  - [ ] Follow-up detection: user→assistant→user(short) counts as follow-up
  - [ ] Correction detection: message with "不是" triggers correction count
  - [ ] Keyword extraction: returns top-5 keywords from CJK text
  - [ ] Language detection: CJK-heavy text → "zh", ASCII-heavy → "en"
- [ ] Test `PredictionEngine::predict()`:
  - [ ] Cold user (no model) → low confidence prediction
  - [ ] Warm user (50+ conversations) → high confidence prediction
  - [ ] Prediction values are in valid ranges [0, 1]
- [ ] Test `PredictionEngine::calculate_error()`:
  - [ ] Perfect prediction → Negligible category
  - [ ] All-wrong prediction → Critical category
  - [ ] Unexpected correction → increases composite_error
  - [ ] composite_error is bounded [0, 1]
- [ ] Test `DualProcessRouter::route()`:
  - [ ] Negligible → None
  - [ ] Moderate → StoreEpisodic
  - [ ] Significant → TriggerReflection
  - [ ] Significant with 3 consecutive → TriggerEmergencyEvolution
  - [ ] Critical → TriggerEmergencyEvolution
- [ ] Test `MetaCognition`:
  - [ ] Default thresholds produce expected categories
  - [ ] After 100 predictions with low effectiveness: thresholds rise
  - [ ] After 100 predictions with high effectiveness: thresholds lower
  - [ ] Thresholds clamped to [0.1, 0.95]
  - [ ] Persistence: save → load roundtrip preserves state

#### 1.7.2 Integration tests `[TST]`

- [ ] Create `crates/duduclaw-gateway/tests/prediction_integration.rs`
  - [ ] Test full flow: predict → conversation → extract metrics → calculate error → route → action
  - [ ] Test SQLite persistence: create engine, predict, restart engine, verify model loaded
  - [ ] Test silence breaker: simulate no conversations for 12h, verify forced Meso trigger
  - [ ] Test backward compat: `prediction_driven=false` → heartbeat triggers unchanged

---

## Phase 2: GVU Self-Play Loop (Evolution Quality Core) ✅ IMPLEMENTED

> Goal: Replace single-pass reflection with a convergent Generator→Verifier→Updater loop.
> Every evolution proposal is verified, versioned, and rollback-capable.
>
> **Status: COMPLETE** (2026-03-27) — 27 tests passing, zero warnings.
>
> ### Implementation Summary
>
> | File | Type | Lines | Description |
> |------|------|:-----:|-------------|
> | `gvu/mod.rs` | NEW | 24 | Module declarations |
> | `gvu/proposal.rs` | NEW | 105 | ProposalType, ProposalStatus, EvolutionProposal |
> | `gvu/text_gradient.rs` | NEW | 85 | TextGradient (Blocking/Advisory), to_prompt_section() |
> | `gvu/version_store.rs` | NEW | 270 | VersionMetrics, SoulVersion, VersionStore + SQLite persistence |
> | `gvu/generator.rs` | NEW | 175 | Generator with OPRO history context, prompt builder, response parser |
> | `gvu/verifier.rs` | NEW | 300 | 4-layer verifier: L1 deterministic, L2 metrics/history, L3 LLM judge, L4 trend |
> | `gvu/updater.rs` | NEW | 210 | Versioned SOUL.md apply, observation period, outcome judge, rollback |
> | `gvu/loop_.rs` | NEW | 220 | GVU orchestrator: max 3 generations, per-agent lock, TextGrad feedback loop |
> | `gvu/tests.rs` | TST | 340 | 27 unit tests across all modules |
> | `lib.rs` | MOD | +1 | pub mod gvu |
> | **Total** | | **~1,730** | |
>
> ### Key Design Decisions
> - **Generator** uses OPRO pattern: includes last 5 version summaries + metrics in prompt
> - **Verifier L1** is zero-cost: catches CONTRACT.toml violations, sensitive patterns, empty/oversized content
> - **Verifier L3** (LLM Judge) is optional: loop works with deterministic layers only when LLM unavailable
> - **Updater** stores full previous SOUL.md as rollback_diff (not a diff — simplifies rollback)
> - **GVU Loop** accepts a generic `call_llm` async closure — backend agnostic (CLI, API, or mock)
> - **Per-agent Mutex** prevents concurrent GVU loops; active observation blocks new loops

### 2.1 Evolution Proposal — Core Data Structures

#### 2.1.1 Define proposal types `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/mod.rs`
  - [ ] Declare submodule exports: `proposal`, `generator`, `verifier`, `updater`, `text_gradient`, `version_store`
- [ ] Create `crates/duduclaw-gateway/src/gvu/proposal.rs`
  - [ ] Define `ProposalType` enum
    - [ ] `SoulPatch` — SOUL.md modification (unified diff)
    - [ ] `SkillAdd` — new skill file content
    - [ ] `SkillArchive` — move skill to archive
    - [ ] `ContractAmend` — CONTRACT.toml modification
  - [ ] Define `ProposalStatus` enum
    - [ ] `Generating` — Generator is producing
    - [ ] `Verifying` — Verifier is evaluating
    - [ ] `Rejected { gradient: TextGradient }` — failed verification
    - [ ] `Approved` — passed all verifier layers
    - [ ] `Applied` — written to disk, in observation period
    - [ ] `Observing` — observation period active
    - [ ] `Confirmed` — observation passed, permanent
    - [ ] `RolledBack { reason: String }` — observation failed, reverted
  - [ ] Define `EvolutionProposal` struct
    - [ ] `id: String` (UUID)
    - [ ] `agent_id: String`
    - [ ] `proposal_type: ProposalType`
    - [ ] `content: String` (diff or full content)
    - [ ] `rationale: String`
    - [ ] `generation: u32` (attempt number, max 3)
    - [ ] `status: ProposalStatus`
    - [ ] `trigger_error: Option<PredictionError>` (what triggered this evolution)
    - [ ] `created_at: DateTime<Utc>`
    - [ ] `resolved_at: Option<DateTime<Utc>>`
  - [ ] Implement `Serialize` / `Deserialize` for all structs

#### 2.1.2 Proposal persistence `[MIG]`

- [ ] Add SQLite migration:
  ```sql
  CREATE TABLE IF NOT EXISTS evolution_proposals (
      id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL,
      proposal_type TEXT NOT NULL,
      content TEXT NOT NULL,
      rationale TEXT NOT NULL,
      generation INTEGER DEFAULT 1,
      status TEXT NOT NULL DEFAULT 'generating',
      trigger_context TEXT,
      created_at TEXT NOT NULL,
      resolved_at TEXT
  );
  CREATE INDEX IF NOT EXISTS idx_proposals_agent ON evolution_proposals(agent_id);
  CREATE INDEX IF NOT EXISTS idx_proposals_status ON evolution_proposals(status);
  ```
- [ ] Implement CRUD functions:
  - [ ] `insert_proposal(proposal: &EvolutionProposal)`
  - [ ] `update_proposal_status(id: &str, status: ProposalStatus)`
  - [ ] `get_active_proposals(agent_id: &str) -> Vec<EvolutionProposal>`
  - [ ] `get_proposal_history(agent_id: &str, limit: usize) -> Vec<EvolutionProposal>`

### 2.2 VersionStore — OPRO-Style Historical Tracking

#### 2.2.1 Define version tracking `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/version_store.rs`
  - [ ] Define `VersionMetrics` struct
    - [ ] `positive_feedback_ratio: f64`
    - [ ] `avg_prediction_error: f64` (from PredictionEngine)
    - [ ] `user_correction_rate: f64`
    - [ ] `contract_violations: u32`
    - [ ] `conversations_count: u32`
  - [ ] Define `SoulVersion` struct
    - [ ] `version_id: String`
    - [ ] `agent_id: String`
    - [ ] `soul_hash: String` (SHA-256)
    - [ ] `soul_summary: String` (first 200 chars or LLM-generated)
    - [ ] `applied_at: DateTime<Utc>`
    - [ ] `observation_end: DateTime<Utc>`
    - [ ] `status: VersionStatus` — Active / Observing / Confirmed / RolledBack
    - [ ] `pre_metrics: VersionMetrics` (baseline before this version)
    - [ ] `post_metrics: Option<VersionMetrics>` (measured after observation)
    - [ ] `proposal_id: String` (link to originating proposal)
    - [ ] `rollback_diff: String` (reverse patch for undo)

#### 2.2.2 Version persistence `[MIG]`

- [ ] Add SQLite migration:
  ```sql
  CREATE TABLE IF NOT EXISTS soul_versions (
      version_id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL,
      soul_hash TEXT NOT NULL,
      soul_summary TEXT NOT NULL,
      applied_at TEXT NOT NULL,
      observation_end TEXT NOT NULL,
      status TEXT NOT NULL DEFAULT 'observing',
      pre_metrics_json TEXT NOT NULL,
      post_metrics_json TEXT,
      proposal_id TEXT NOT NULL,
      rollback_diff TEXT NOT NULL
  );
  CREATE INDEX IF NOT EXISTS idx_versions_agent ON soul_versions(agent_id);
  CREATE INDEX IF NOT EXISTS idx_versions_status ON soul_versions(status);
  ```

#### 2.2.3 Implement VersionStore `[NEW]`

- [ ] In `version_store.rs`:
  - [ ] Implement `VersionStore` struct with SQLite connection
  - [ ] Implement `record_version(version: &SoulVersion)`
  - [ ] Implement `get_current_version(agent_id: &str) -> Option<SoulVersion>`
  - [ ] Implement `get_history(agent_id: &str, limit: usize) -> Vec<SoulVersion>`
    - [ ] Ordered by applied_at DESC
    - [ ] Used by Generator for OPRO-style history context
  - [ ] Implement `mark_confirmed(version_id: &str)`
  - [ ] Implement `mark_rolled_back(version_id: &str, reason: &str)`
  - [ ] Implement `collect_metrics_since(agent_id: &str, since: DateTime<Utc>) -> VersionMetrics`
    - [ ] Query prediction_errors, feedback signals, contract violations since timestamp
    - [ ] Aggregate into VersionMetrics

### 2.3 TextGradient — Structured Feedback Signals

#### 2.3.1 Define TextGradient `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/text_gradient.rs`
  - [ ] Define `TextGradient` struct
    - [ ] `target: String` (which part of proposal, e.g., "SOUL.md lines 15-18")
    - [ ] `critique: String` (what's wrong)
    - [ ] `suggestion: String` (specific fix suggestion)
    - [ ] `source_layer: String` (which verifier layer produced this)
    - [ ] `severity: GradientSeverity` — Blocking / Advisory
  - [ ] Define `GradientSeverity` enum
    - [ ] `Blocking` — must fix before approval
    - [ ] `Advisory` — suggestion, Generator may ignore
  - [ ] Implement `TextGradient::to_prompt_section(&self) -> String`
    - [ ] Format as markdown for injection into Generator re-prompt

### 2.4 Generator — OPRO-Informed Proposal Generation

#### 2.4.1 Define evolution LLM client `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/llm_client.rs`
  - [ ] Define `EvolutionLlmClient` struct
    - [ ] `http: reqwest::Client`
    - [ ] `api_key_provider: Arc<dyn Fn() -> String + Send + Sync>` (from account_rotator)
  - [ ] Implement `call_with_tool_use<T: DeserializeOwned>(model, system, user_msg, tool_schema) -> Result<T>`
    - [ ] POST to `https://api.anthropic.com/v1/messages`
    - [ ] Set `anthropic-version: 2023-06-01`
    - [ ] Use `tool_choice: { "type": "tool", "name": "<name>" }` to force structured output
    - [ ] Parse tool_use content block → deserialize to T
    - [ ] Handle rate limits: retry with exponential backoff (max 2 retries)
    - [ ] Handle errors: return descriptive error with status code
  - [ ] Implement `call_text(model, system, user_msg) -> Result<String>`
    - [ ] Simple text response for LLM-as-Judge

#### 2.4.2 Implement Generator `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/generator.rs`
  - [ ] Define `Generator` struct
    - [ ] `llm: Arc<EvolutionLlmClient>`
    - [ ] `version_store: Arc<VersionStore>`
  - [ ] Define `GeneratorInput` struct
    - [ ] `agent_id: String`
    - [ ] `agent_soul: String` (current SOUL.md content)
    - [ ] `agent_contract: Option<Contract>` (current CONTRACT.toml)
    - [ ] `trigger_context: String` (prediction error details, external factors)
    - [ ] `previous_gradients: Vec<TextGradient>` (from failed verification attempts)
    - [ ] `generation: u32` (attempt number)
  - [ ] Implement `generate(&self, input: &GeneratorInput) -> Result<EvolutionProposal>`
    - [ ] Load version history from VersionStore (last 5 versions with metrics)
    - [ ] Build OPRO-style prompt:
      - [ ] Include version history with metrics (scored historical context)
      - [ ] Include current SOUL.md
      - [ ] Include trigger context (prediction errors, external signals)
      - [ ] If generation > 1: include previous TextGradients as "previous attempt feedback"
      - [ ] Instruction: generate unified diff patch for SOUL.md
    - [ ] Call `llm.call_with_tool_use::<ProposalOutput>("claude-haiku-4-5", ...)`
    - [ ] Construct EvolutionProposal from LLM output
  - [ ] Define `ProposalOutput` struct (tool_use schema)
    - [ ] `diff: String` (unified diff format)
    - [ ] `rationale: String`
    - [ ] `expected_improvement: String` (what metric should improve)

### 2.5 Verifier — Multi-Layer Evaluation

#### 2.5.1 Implement Layer 1: Deterministic Rules `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/verifier.rs`
  - [ ] Define `VerificationResult` enum
    - [ ] `Approved { confidence: f64 }`
    - [ ] `Rejected { gradient: TextGradient }`
  - [ ] Define `DeterministicVerifier`
  - [ ] Implement deterministic checks:
    - [ ] Diff is valid unified diff format (parseable)
    - [ ] Diff applies cleanly to current SOUL.md
    - [ ] No `must_not` CONTRACT.toml patterns appear in proposed new SOUL.md
    - [ ] All `must_always` patterns still present in proposed new SOUL.md
    - [ ] Diff does not remove >50% of SOUL.md content (safety guard)
    - [ ] No sensitive patterns in diff (API keys, secrets, file paths with credentials)
    - [ ] Character count of new SOUL.md within [100, 10000] range
  - [ ] Return `TextGradient` with specific violation details on failure

#### 2.5.2 Implement Layer 2: Metrics Prediction `[NEW]`

- [ ] In `verifier.rs`:
  - [ ] Define `MetricsPredictionVerifier`
  - [ ] Implement prediction based on historical versions:
    - [ ] Load past versions with similar diff patterns (keyword overlap)
    - [ ] If similar past diff was rolled back → reject with gradient "similar change was rolled back: {reason}"
    - [ ] If no historical data → pass (no evidence to reject)
    - [ ] If historical data shows negative trend for similar changes → warn (Advisory gradient)

#### 2.5.3 Implement Layer 3: LLM-as-Judge `[NEW]`

- [ ] In `verifier.rs`:
  - [ ] Define `LlmJudgeVerifier`
  - [ ] Implement LLM evaluation:
    - [ ] Build judge prompt:
      - [ ] Include CONTRACT.toml boundaries as evaluation criteria
      - [ ] Include current SOUL.md + proposed diff
      - [ ] Ask: "Does this change violate any boundaries? Is it coherent? Will it improve the agent?"
    - [ ] Call `llm.call_with_tool_use::<JudgeOutput>("claude-haiku-4-5", ...)`
    - [ ] `JudgeOutput`: `{ approved: bool, score: f64, feedback: String }`
    - [ ] If score < 0.7 → reject with feedback as TextGradient
    - [ ] If score >= 0.7 → approve with confidence = score

#### 2.5.4 Implement Layer 4: Trend Consistency `[NEW]`

- [ ] In `verifier.rs`:
  - [ ] Define `TrendConsistencyVerifier`
  - [ ] Implement trend checks:
    - [ ] Load version history (last 5 confirmed versions)
    - [ ] If this proposal reverses direction of last confirmed version → Advisory gradient
    - [ ] If this proposal repeats a rolled-back version's change → Blocking gradient
    - [ ] Check for "oscillation": if last 3 versions flip-flop on same section → reject

#### 2.5.5 Compose MultiLayerVerifier `[NEW]`

- [ ] In `verifier.rs`:
  - [ ] Define `MultiLayerVerifier` struct combining L1-L4
  - [ ] Implement `verify(&self, proposal: &EvolutionProposal) -> VerificationResult`
    - [ ] Run L1 (deterministic) — if Blocking reject → return immediately
    - [ ] Run L2 (metrics prediction) — if Blocking reject → return
    - [ ] Run L3 (LLM judge) — if reject → return
    - [ ] Run L4 (trend) — if Blocking reject → return
    - [ ] Collect all Advisory gradients → attach to Approved result
    - [ ] Return Approved with min(L3.confidence, L2.confidence)

### 2.6 Updater — Versioned Application + Observation Period

#### 2.6.1 Implement Updater `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/updater.rs`
  - [ ] Define `Updater` struct
    - [ ] `version_store: Arc<VersionStore>`
    - [ ] `observation_duration: Duration` (default 24h, configurable in agent.toml)
  - [ ] Implement `apply(&self, proposal: &EvolutionProposal, agent_dir: &Path) -> Result<SoulVersion>`
    - [ ] Read current SOUL.md
    - [ ] Apply diff patch to produce new content
    - [ ] Validate result is valid UTF-8 and non-empty
    - [ ] Compute pre_metrics (collect current metrics from VersionStore)
    - [ ] Compute rollback_diff (reverse of the applied diff)
    - [ ] Write new SOUL.md to disk
    - [ ] Call `accept_soul_change()` (soul_guard.rs:156) to update SHA-256 hash + save version history
    - [ ] Create SoulVersion record with status=Observing
    - [ ] Store in VersionStore
    - [ ] Schedule observation end event (tokio::time::sleep_until or register in heartbeat)
    - [ ] Return SoulVersion

#### 2.6.2 Implement Outcome Judge `[NEW]`

- [ ] In `updater.rs`:
  - [ ] Implement `judge_outcome(&self, version: &SoulVersion) -> OutcomeVerdict`
    - [ ] Collect post_metrics since version.applied_at
    - [ ] Compare post_metrics vs pre_metrics:
      - [ ] positive_feedback_ratio: tolerance -0.03 (allow 3% dip)
      - [ ] avg_prediction_error: tolerance +0.05 (allow 5% increase)
      - [ ] contract_violations: must not increase
    - [ ] If all within tolerance → `Confirm`
    - [ ] If any metric significantly worse → `Rollback { reason }`
    - [ ] If insufficient data (< 5 conversations in observation period) → `ExtendObservation { extra: Duration }`
  - [ ] Define `OutcomeVerdict` enum
    - [ ] `Confirm`
    - [ ] `Rollback { reason: String }`
    - [ ] `ExtendObservation { extra: Duration }`
  - [ ] Implement `execute_rollback(&self, version: &SoulVersion, agent_dir: &Path) -> Result<()>`
    - [ ] Apply rollback_diff to current SOUL.md
    - [ ] Update soul_guard hash
    - [ ] Mark version as RolledBack in VersionStore
    - [ ] Log at WARN level
    - [ ] Notify MetaCognition (record_outcome with improved=false)

### 2.7 GVU Loop Orchestrator

#### 2.7.1 Implement main loop `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/loop_.rs`
  - [ ] Define `GvuLoop` struct
    - [ ] `generator: Generator`
    - [ ] `verifier: MultiLayerVerifier`
    - [ ] `updater: Updater`
    - [ ] `max_generations: u32` (default 3)
  - [ ] Implement `run(&self, input: GeneratorInput) -> Result<GvuOutcome>`
    - [ ] Loop up to max_generations:
      - [ ] Call `generator.generate(input_with_gradients)`
      - [ ] Update proposal status to `Verifying`
      - [ ] Call `verifier.verify(proposal)`
      - [ ] If Approved:
        - [ ] Call `updater.apply(proposal, agent_dir)`
        - [ ] Return `GvuOutcome::Applied(version)`
      - [ ] If Rejected:
        - [ ] Collect TextGradient from rejection
        - [ ] Add to input.previous_gradients
        - [ ] Increment generation counter
        - [ ] Continue loop
    - [ ] If exhausted all generations:
      - [ ] Return `GvuOutcome::Abandoned { last_gradient }`
      - [ ] Log at INFO level
  - [ ] Define `GvuOutcome` enum
    - [ ] `Applied(SoulVersion)`
    - [ ] `Abandoned { last_gradient: TextGradient }`
    - [ ] `Skipped { reason: String }` (e.g., already has active observation)

#### 2.7.2 Guard: prevent concurrent evolution `[NEW]`

- [ ] In `loop_.rs`:
  - [ ] Before running GVU loop: check if agent already has a version in `Observing` status
    - [ ] If yes → return `GvuOutcome::Skipped("active observation period")`
  - [ ] Use per-agent Mutex to prevent two GVU loops running simultaneously

#### 2.7.3 Wire observation period completion `[MOD]`

- [ ] In heartbeat scheduler (heartbeat.rs):
  - [ ] Add periodic check (every 5 minutes) for versions past observation_end:
    - [ ] Query VersionStore for `status=observing AND observation_end < now`
    - [ ] For each: call `updater.judge_outcome(version)`
    - [ ] Execute verdict (confirm, rollback, or extend)
    - [ ] Notify MetaCognition of outcome

### 2.8 Connect Phase 1 → Phase 2

#### 2.8.1 Replace Phase 1 placeholder with GVU `[MOD]`

- [ ] In `router.rs` (Phase 1):
  - [ ] `TriggerReflection` action: invoke `GvuLoop::run()` instead of `run_meso()`
  - [ ] `TriggerEmergencyEvolution` action: invoke `GvuLoop::run()` with shortened observation period (6h instead of 24h)
- [ ] In `channel_reply.rs`:
  - [ ] Add `GvuLoop` to `ReplyContext` struct
  - [ ] Spawn GVU loop as background tokio task (don't block response)

#### 2.8.2 MCP tool integration `[MOD]`

- [ ] Add MCP tools in `mcp.rs`:
  - [ ] `evolution_status(agent_id)` — return current proposal status, active observation, version history
  - [ ] `evolution_history(agent_id, limit)` — return past proposals with outcomes
  - [ ] `evolution_rollback(agent_id)` — manually trigger rollback of current observing version
  - [ ] `evolution_confirm(agent_id)` — manually confirm current observing version (skip observation)

### 2.9 Phase 2 Tests

#### 2.9.1 Unit tests `[TST]`

- [ ] Create `crates/duduclaw-gateway/src/gvu/tests/mod.rs`
- [ ] Test `TextGradient::to_prompt_section()`:
  - [ ] Produces valid markdown
  - [ ] Includes target, critique, and suggestion
- [ ] Test `DeterministicVerifier`:
  - [ ] Valid diff passes
  - [ ] Invalid diff syntax rejected
  - [ ] CONTRACT.toml must_not violation → Blocking gradient with specific violation
  - [ ] >50% content removal → Blocking gradient
  - [ ] Sensitive pattern detection (API key regex)
- [ ] Test `MetricsPredictionVerifier`:
  - [ ] No history → pass
  - [ ] Similar rolled-back change → reject
  - [ ] Dissimilar change → pass
- [ ] Test `TrendConsistencyVerifier`:
  - [ ] Repeating rolled-back change → Blocking
  - [ ] Oscillation detection (3 flip-flops) → reject
  - [ ] Normal progression → pass
- [ ] Test `MultiLayerVerifier` composition:
  - [ ] L1 fail → early return, L3 not called (no LLM cost)
  - [ ] L1 pass + L3 fail → rejected with L3 gradient
  - [ ] All pass → Approved with combined confidence
- [ ] Test `Updater::apply()`:
  - [ ] Diff applies correctly to SOUL.md
  - [ ] Rollback diff correctly reverses the change
  - [ ] SoulVersion record created with correct pre_metrics
- [ ] Test `Updater::judge_outcome()`:
  - [ ] Metrics improved → Confirm
  - [ ] Metrics within tolerance → Confirm
  - [ ] Metrics significantly worse → Rollback
  - [ ] Insufficient data (< 5 conversations) → ExtendObservation
- [ ] Test `GvuLoop::run()`:
  - [ ] Pass on first attempt → Applied
  - [ ] Fail once, pass on retry → Applied (generation=2)
  - [ ] Fail 3 times → Abandoned
  - [ ] Active observation → Skipped
- [ ] Test `VersionStore`:
  - [ ] Insert + query roundtrip
  - [ ] History returns correct order (newest first)
  - [ ] collect_metrics_since aggregates correctly

#### 2.9.2 Integration tests `[TST]`

- [ ] Create `crates/duduclaw-gateway/tests/gvu_integration.rs`
  - [ ] Test full GVU cycle with mock LLM (deterministic responses):
    - [ ] Generator produces diff → Verifier approves → Updater applies
    - [ ] Verify SOUL.md on disk changed
    - [ ] Verify version recorded in SQLite
  - [ ] Test rollback cycle:
    - [ ] Apply version → simulate bad metrics → judge rollback → verify SOUL.md restored
  - [ ] Test OPRO context:
    - [ ] After 3 confirmed versions, verify Generator prompt includes all 3 with metrics

---

## Phase 3: Cognitive Memory Layer (Structured Memory) ✅ IMPLEMENTED

> Goal: Split flat `memories` table into episodic/semantic layers with importance scoring.
> Minimal change: add `layer` column + importance scoring, not full four-layer rewrite.
>
> **Status: COMPLETE** (2026-03-27) — 24 memory tests + 63 gateway tests = 87 total, all passing.
>
> ### Implementation Summary
>
> | File | Type | Lines | Description |
> |------|------|:-----:|-------------|
> | `duduclaw-core/types.rs` | MOD | +55 | `MemoryLayer` enum (Episodic/Semantic/Procedural) + 5 new fields on `MemoryEntry` |
> | `duduclaw-memory/router.rs` | NEW | 150 | Rule-based layer classification + importance scoring (8 tests) |
> | `duduclaw-memory/engine.rs` | MOD | +110 | Schema migration (5 new columns), importance-weighted re-ranking (Generative Agents 3D weighting), `search_layer()`, `episodic_pressure()`, `semantic_conflict_count()` |
> | `duduclaw-memory/search.rs` | MOD | +5 | MemoryEntry field compat |
> | `duduclaw-memory/lib.rs` | MOD | +2 | pub mod router, pub use classify |
> | `duduclaw-cli/mcp.rs` | MOD | +10 | MemoryEntry field compat (2 sites) |
> | **Total** | | **~330** | |
>
> ### Key Design Decisions
> - **Schema migration is idempotent**: `ALTER TABLE ADD COLUMN` errors silently ignored for existing DBs
> - **Search re-ranking**: FTS5 returns 4x candidates, then re-ranked by `0.25*recency + 0.35*importance + 0.40*fts_rank` (Generative Agents paper)
> - **Access tracking**: `access_count` and `last_accessed` updated on every search hit (feeds recency decay)
> - **Router is zero-cost**: pure rule-based, no LLM calls, supports both English and Chinese indicators

### 3.1 Memory Layer Classification

#### 3.1.1 Extend memory schema `[MIG]`

- [ ] Add migration to `crates/duduclaw-memory/src/engine.rs`:
  ```sql
  ALTER TABLE memories ADD COLUMN layer TEXT NOT NULL DEFAULT 'episodic';
  ALTER TABLE memories ADD COLUMN importance REAL NOT NULL DEFAULT 5.0;
  ALTER TABLE memories ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
  ALTER TABLE memories ADD COLUMN last_accessed TEXT;
  ALTER TABLE memories ADD COLUMN source_event TEXT DEFAULT '';
  CREATE INDEX IF NOT EXISTS idx_memories_layer ON memories(layer);
  CREATE INDEX IF NOT EXISTS idx_memories_importance ON memories(importance DESC);
  ```
- [ ] Handle migration for existing rows: set `layer='episodic'`, `importance=5.0`

#### 3.1.2 Extend MemoryEntry struct `[MOD]`

- [ ] Modify `MemoryEntry` in types.rs:188
  - [ ] Add `layer: MemoryLayer` field
  - [ ] Add `importance: f64` field (0.0 - 10.0)
  - [ ] Add `access_count: u32` field
  - [ ] Add `last_accessed: Option<DateTime<Utc>>` field
  - [ ] Add `source_event: String` field
- [ ] Define `MemoryLayer` enum in types.rs:
  - [ ] `Episodic` — conversation summaries, reflection conclusions, failure traces
  - [ ] `Semantic` — generalized knowledge, user preference models, domain rules
  - [ ] `Procedural` — reserved for future (skills, SOUL.md — tracked separately)
- [ ] Update all existing `MemoryEntry` construction sites to include new fields with defaults

### 3.2 Memory Router

#### 3.2.1 Implement automatic layer classification `[NEW]`

- [ ] Create `crates/duduclaw-memory/src/router.rs`
  - [ ] Define `MemoryRouter`
  - [ ] Implement `classify(content: &str, source: &str) -> (MemoryLayer, f64)`
    - [ ] Rule-based classification:
      - [ ] Source is "micro_reflection" or "conversation_summary" → Episodic, importance 5.0
      - [ ] Source is "meso_reflection" → Episodic, importance 7.0
      - [ ] Source is "macro_reflection" → Semantic, importance 8.0
      - [ ] Source is "user_feedback" → Episodic, importance by feedback type (positive=4, negative=7, correction=8)
      - [ ] Source is "user_preference" or "generalized_rule" → Semantic, importance 6.0
      - [ ] Content contains patterns like "always", "never", "rule:", "principle:" → Semantic, importance +1
      - [ ] Default → Episodic, importance 5.0
    - [ ] Clamp importance to [1.0, 10.0]

### 3.3 Importance-Weighted Retrieval

#### 3.3.1 Modify search to use recency + importance `[MOD]`

- [ ] Modify `SqliteMemoryEngine::search()` (engine.rs:160)
  - [ ] Current: FTS5 MATCH query ordered by rank
  - [ ] New: FTS5 MATCH + post-retrieval scoring:
    - [ ] Retrieve top 20 candidates from FTS5
    - [ ] For each: compute `score = α * recency + β * importance + γ * fts_rank`
      - [ ] `recency = 0.99^hours_since_last_access`
      - [ ] `importance = memory.importance / 10.0`
      - [ ] `fts_rank = normalized FTS5 rank`
      - [ ] Default weights: α=0.25, β=0.35, γ=0.40
    - [ ] Re-sort by combined score
    - [ ] Return top `limit` results
    - [ ] Update `access_count` and `last_accessed` for returned results

#### 3.3.2 Add layer-filtered search `[NEW]`

- [ ] Add `search_layer()` method to `SqliteMemoryEngine`:
  - [ ] `search_layer(agent_id, query, layer: MemoryLayer, limit) -> Vec<MemoryEntry>`
  - [ ] Same as search() but with `WHERE layer = ?` filter
  - [ ] Used by evolution engine to search only semantic memories for existing knowledge

### 3.4 Reflection Output Classification

#### 3.4.1 Modify Micro reflection output handling `[MOD]`

- [ ] Modify `run_micro()` in evolution.rs:17
  - [ ] After receiving micro reflection result:
    - [ ] `what_went_well` → store as Episodic, importance 4.0
    - [ ] `patterns_noticed` → store as Episodic, importance 6.0
    - [ ] `candidate_skills` → store as Episodic, importance 7.0

#### 3.4.2 Modify Meso reflection output handling `[MOD]`

- [ ] Modify `run_meso()` in evolution.rs:42
  - [ ] After receiving meso reflection result:
    - [ ] `common_patterns` → check if similar semantic memory exists:
      - [ ] If similar exists (FTS5 match > 0.8): update existing memory's importance +1
      - [ ] If new: store as **Semantic**, importance 7.0
    - [ ] `memory_updates` → store as Semantic, importance 6.0

#### 3.4.3 Modify Macro reflection output handling `[MOD]`

- [ ] Modify `run_macro()` in evolution.rs:58
  - [ ] After receiving macro reflection result:
    - [ ] `recommendations` → store as Semantic, importance 8.0
    - [ ] `report` → store as Episodic, importance 5.0 (historical record)

### 3.5 Memory Pressure for Reflection Gate (Connect to Phase 1)

#### 3.5.1 Implement memory pressure calculation `[NEW]`

- [ ] Add to `crates/duduclaw-memory/src/engine.rs`:
  - [ ] Implement `episodic_pressure(agent_id: &str) -> f64`
    - [ ] Count episodic memories created since last Meso reflection
    - [ ] Sum their importance values
    - [ ] Return weighted count: `Σ(importance_i) / 10.0`
  - [ ] Implement `semantic_conflict_count(agent_id: &str) -> u32`
    - [ ] Count recent episodic memories that contradict existing semantic memories
    - [ ] Simple heuristic: episodic memory with importance >= 7 and no matching semantic memory
    - [ ] Returns count (used as secondary evolution trigger)

#### 3.5.2 Feed memory pressure into prediction error `[MOD]`

- [ ] In PredictionEngine (Phase 1):
  - [ ] After calculating composite_error:
    - [ ] Query episodic_pressure for agent
    - [ ] If episodic_pressure > 10.0: boost composite_error by 0.1 (encourage Meso reflection)
    - [ ] This creates a natural "memory needs consolidation" signal

### 3.6 Phase 3 Tests

#### 3.6.1 Unit tests `[TST]`

- [ ] Create `crates/duduclaw-memory/tests/cognitive_memory.rs`
- [ ] Test `MemoryRouter::classify()`:
  - [ ] micro_reflection source → Episodic
  - [ ] macro_reflection source → Semantic
  - [ ] Content with "always" keyword → Semantic, importance +1
  - [ ] Importance clamped to [1.0, 10.0]
- [ ] Test importance-weighted search:
  - [ ] High importance + recent → ranked first
  - [ ] Low importance + old → ranked last
  - [ ] access_count incremented after search
  - [ ] last_accessed updated after search
- [ ] Test layer-filtered search:
  - [ ] search_layer(Semantic) returns only Semantic memories
  - [ ] search_layer(Episodic) returns only Episodic memories
- [ ] Test episodic_pressure:
  - [ ] No memories → pressure 0.0
  - [ ] 10 memories with importance 5.0 each → pressure 5.0
- [ ] Test migration:
  - [ ] Existing memories get layer=episodic, importance=5.0

#### 3.6.2 Integration tests `[TST]`

- [ ] Create `crates/duduclaw-memory/tests/cognitive_integration.rs`
  - [ ] Test full flow: store episodic → store semantic → search returns semantic first (higher importance)
  - [ ] Test reflection output classification: mock micro result → verify correct layer assignment
  - [ ] Test memory pressure → evolution trigger chain

---

## Phase 4: Integration, Polish, and Migration ✅ IMPLEMENTED

> **Status: COMPLETE** (2026-03-27) — 87 tests passing, zero warnings, all crates build clean.
>
> ### Implementation Summary
>
> | File | Type | Description |
> |------|------|-------------|
> | `types.rs` | MOD | Added `max_gvu_generations`, `observation_period_hours` to EvolutionConfig |
> | `server.rs` | MOD | Initialize PredictionEngine + GvuLoop at gateway startup, wire into ReplyContext |
> | `channel_reply.rs` | MOD | `with_gvu_loop()` builder, GVU loop wired into TriggerReflection/Emergency actions, `call_claude_cli_public()` for GVU LLM calls |
> | `evolution.rs` | MOD | Default values for 2 new EvolutionConfig fields |
> | `mcp.rs` | MOD | create_agent writes new evolution fields to agent.toml |
> | `CLAUDE.md` | MOD | Architecture section documents Evolution Engine v2 |
>
> ### Integration Flow (End-to-End)
> ```
> Gateway startup (server.rs)
>   ├─ PredictionEngine::new(prediction.db, metacognition.json)
>   ├─ GvuLoop::new(evolution.db)
>   └─ ReplyContext.with_prediction_engine().with_gvu_loop()
>
> Conversation completes (channel_reply.rs)
>   ├─ PredictionEngine.predict() → PredictionError
>   ├─ DualProcessRouter.route()
>   │   ├─ Negligible → (nothing)
>   │   ├─ Moderate → store episodic
>   │   ├─ Significant → GvuLoop.run() with claude-haiku-4-5
>   │   └─ Critical → GvuLoop.run() (emergency)
>   └─ Fallback: legacy run_meso()/run_macro() when GVU unavailable
> ```

### 4.1 End-to-End Wiring

- [ ] Create unified initialization in gateway `server.rs`:
  - [ ] Initialize PredictionEngine with SQLite path
  - [ ] Initialize GvuLoop with EvolutionLlmClient + VersionStore + MultiLayerVerifier
  - [ ] Initialize MemoryRouter
  - [ ] Pass all into ReplyContext factory
- [ ] Wire shutdown hooks:
  - [ ] Persist MetaCognition thresholds
  - [ ] Persist all UserModels
  - [ ] Flush pending proposals

### 4.2 Configuration

#### 4.2.1 Update agent.toml schema `[CFG]`

- [ ] Add new evolution config section:
  ```toml
  [evolution]
  prediction_driven = true          # enable Phase 1
  gvu_enabled = true                # enable Phase 2
  cognitive_memory = true           # enable Phase 3
  max_gvu_generations = 3
  observation_period_hours = 24
  max_silence_hours = 12.0

  [evolution.thresholds]
  negligible_upper = 0.2
  moderate_upper = 0.5
  significant_upper = 0.8

  [evolution.gvu]
  model_generator = "claude-haiku-4-5"
  model_judge = "claude-haiku-4-5"
  ```

#### 4.2.2 Update EvolutionConfig struct `[MOD]`

- [ ] Add fields to `EvolutionConfig` (types.rs:94):
  - [ ] `prediction_driven: bool`
  - [ ] `gvu_enabled: bool`
  - [ ] `cognitive_memory: bool`
  - [ ] `max_gvu_generations: u32`
  - [ ] `observation_period_hours: f64`
  - [ ] `max_silence_hours: f64`
- [ ] Add `ThresholdsConfig` struct
- [ ] Add `GvuModelConfig` struct

### 4.3 Migration Path

- [ ] Ensure all new features are behind feature flags (default off)
- [ ] Write migration guide:
  - [ ] Step 1: Update binary
  - [ ] Step 2: Set `prediction_driven = true` in agent.toml
  - [ ] Step 3: Run 50+ conversations to warm up UserModels
  - [ ] Step 4: Set `gvu_enabled = true`
  - [ ] Step 5: Set `cognitive_memory = true`
- [ ] Old Python evolution code remains functional when flags are off

### 4.4 Cleanup (After Stabilization)

- [ ] Mark `python/duduclaw/evolution/micro.py` as deprecated (replaced by Phase 1 rules)
- [ ] Mark `python/duduclaw/evolution/meso.py` as deprecated (replaced by Phase 2 GVU)
- [ ] Keep `python/duduclaw/evolution/macro_.py` as fallback for non-GVU mode
- [ ] Update CLAUDE.md architecture section to document new evolution engine
- [ ] Update README.md with new architecture diagram

### 4.5 Dashboard Integration (Future)

- [ ] Add evolution dashboard page:
  - [ ] Display current prediction accuracy (MetaCognition stats)
  - [ ] Display adaptive thresholds (current values)
  - [ ] Display version history with metrics charts
  - [ ] Display active proposals and their GVU loop status
  - [ ] Display observation period countdown
  - [ ] Manual rollback button
  - [ ] Manual confirm button

---

## Summary

| Phase | New Files | Modified Files | New Lines | Est. Test Lines |
|:-----:|:---------:|:--------------:|:---------:|:---------------:|
| 1     | 6         | 4              | ~1,400    | ~450            |
| 2     | 8         | 4              | ~1,800    | ~500            |
| 3     | 1         | 4              | ~400      | ~300            |
| 4     | 0         | 5              | ~200      | —               |
| **Total** | **15** | **17**      | **~3,800**| **~1,250**      |

### File Index

**New Files:**
1. `crates/duduclaw-gateway/src/prediction/mod.rs`
2. `crates/duduclaw-gateway/src/prediction/user_model.rs`
3. `crates/duduclaw-gateway/src/prediction/metrics.rs`
4. `crates/duduclaw-gateway/src/prediction/engine.rs`
5. `crates/duduclaw-gateway/src/prediction/router.rs`
6. `crates/duduclaw-gateway/src/prediction/metacognition.rs`
7. `crates/duduclaw-gateway/src/gvu/mod.rs`
8. `crates/duduclaw-gateway/src/gvu/proposal.rs`
9. `crates/duduclaw-gateway/src/gvu/version_store.rs`
10. `crates/duduclaw-gateway/src/gvu/text_gradient.rs`
11. `crates/duduclaw-gateway/src/gvu/llm_client.rs`
12. `crates/duduclaw-gateway/src/gvu/generator.rs`
13. `crates/duduclaw-gateway/src/gvu/verifier.rs`
14. `crates/duduclaw-gateway/src/gvu/updater.rs`
15. `crates/duduclaw-gateway/src/gvu/loop_.rs`
16. `crates/duduclaw-memory/src/router.rs`

**Modified Files:**
1. `crates/duduclaw-core/src/types.rs` — EvolutionConfig, MemoryEntry, MemoryLayer
2. `crates/duduclaw-gateway/src/channel_reply.rs` — prediction + GVU integration
3. `crates/duduclaw-gateway/src/evolution.rs` — reflection output classification
4. `crates/duduclaw-gateway/src/session.rs` — init prediction tables
5. `crates/duduclaw-agent/src/heartbeat.rs` — prediction_driven mode + observation checker
6. `crates/duduclaw-memory/src/engine.rs` — importance-weighted search, layer filter
7. `crates/duduclaw-security/src/soul_guard.rs` — integrate versioned updates
8. `crates/duduclaw-cli/src/mcp.rs` — new MCP tools for evolution status
