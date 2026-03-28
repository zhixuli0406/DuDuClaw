# Skill Lifecycle Pipeline — Progressive Injection + Feedback-Driven + Distillation

> **Status: FULLY IMPLEMENTED** (2026-03-27) — 119 tests passing (95 gateway + 24 memory), zero warnings.
>
> ### Implementation Summary
>
> | File | Type | Lines | Phase | Description |
> |------|------|:-----:|:-----:|-------------|
> | `skill_lifecycle/mod.rs` | NEW | 15 | — | Module declarations |
> | `skill_lifecycle/compression.rs` | NEW | 100 | A | CompressedSkill (3-layer), CompressedSkillCache |
> | `skill_lifecycle/relevance.rs` | NEW | 135 | A | Keyword Jaccard ranking + layer selection (CJK bigram) |
> | `skill_lifecycle/diagnostician.rs` | NEW | 120 | B | ErrorCause diagnosis, SkillGap detection |
> | `skill_lifecycle/activation.rs` | NEW | 155 | B | SkillActivationController (activate/deactivate/evaluate) |
> | `skill_lifecycle/gap.rs` | NEW | 40 | B | SkillGap → feedback.jsonl injection |
> | `skill_lifecycle/lift.rs` | NEW | 105 | C | A/B lift measurement (errors_with vs errors_without) |
> | `skill_lifecycle/distillation.rs` | NEW | 100 | C | Readiness scoring + GVU SoulPatch input builder |
> | `skill_lifecycle/tests.rs` | TST | 275 | — | 23 tests across all modules |
> | `lib.rs` | MOD | +1 | — | pub mod skill_lifecycle |
> | `channel_reply.rs` | MOD | +120 | A+B+C | Progressive build_system_prompt, skill lifecycle in prediction flow |
> | `types.rs` | MOD | +12 | D | skill_token_budget, max_active_skills in EvolutionConfig |
> | **Total** | | **~1,178** | |
>
> ### Integration Flow (End-to-End)
> ```
> Conversation arrives
>   ├─ Refresh CompressedSkillCache from agent SKILLS/
>   ├─ Get active skills from SkillActivationController
>   └─ build_system_prompt (progressive):
>       ├─ Layer 0: all skill names (~5 tok each)
>       ├─ Layer 2: active + highly relevant → full content
>       └─ Layer 1: moderately relevant → summary only
>
> After conversation (in prediction spawn):
>   ├─ PredictionError calculated
>   ├─ Diagnose error → suggested_skills or SkillGap
>   ├─ Activate suggested skills
>   ├─ SkillGap → feedback.jsonl → evolution engine
>   ├─ Record conversation for activation effectiveness
>   ├─ Track lift (errors_with vs errors_without per skill)
>   ├─ Every 20 conversations:
>   │   ├─ Evaluate + prune ineffective skills
>   │   └─ Scan distillation candidates (readiness ≥ 0.75)
>   └─ Route to evolution action (GVU loop)
> ```

> Combined architecture: **E (Progressive Injection)** for token efficiency + **F (Feedback-Driven Activation)** for precision + **D (Skill Distillation)** for long-term convergence.
>
> Complete event flow:
> ```
> PredictionError → Diagnose → Activate skill (or SkillGap → evolution)
>                               ↓
>                    Progressive inject (Layer 0/1/2)
>                               ↓
>                    Measure lift → Keep or deactivate
>                               ↓
>                    Mature skill (50+ uses, positive lift)
>                               ↓
>                    Distill into SOUL.md via GVU SoulPatch
>                               ↓
>                    Archive skill file, release token budget
> ```

---

## Phase A: Progressive Injection (Token Efficiency)

> Goal: Replace full skill injection with 3-layer progressive loading.
> 20 skills: ~4000 tokens → ~300 tokens.

### A.1 Skill Compression

#### A.1.1 Define CompressedSkill struct `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/mod.rs`
  - [ ] Declare submodules: `compression`, `relevance`, `activation`, `diagnostician`, `distillation`, `lift`
- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/compression.rs`
  - [ ] Define `CompressedSkill` struct
    - [ ] `name: String`
    - [ ] `tag: String` — Layer 0 (~5 tokens, just the name)
    - [ ] `summary: String` — Layer 1 (~30 tokens, description or first 2 lines)
    - [ ] `full_content: String` — Layer 2 (original markdown)
    - [ ] `tokens_layer0: u32`
    - [ ] `tokens_layer1: u32`
    - [ ] `tokens_layer2: u32`
  - [ ] Implement `CompressedSkill::compress(skill: &SkillFile) -> Self`
    - [ ] tag = skill.name
    - [ ] summary = YAML description field, or first 2 lines of content
    - [ ] full_content = skill.content
    - [ ] Estimate tokens for each layer using `estimate_tokens()`
  - [ ] Implement `Serialize` / `Deserialize` for caching

#### A.1.2 Compression cache `[NEW]`

- [ ] In `compression.rs`:
  - [ ] Implement `compress_all(skills: &[SkillFile]) -> Vec<CompressedSkill>`
  - [ ] Cache compressed skills in memory (recompute when SKILLS/ dir mtime changes)
  - [ ] Add `CompressedSkillCache` struct with `HashMap<String, CompressedSkill>`
  - [ ] Implement `refresh_if_stale(&mut self, skills_dir: &Path, skills: &[SkillFile])`

### A.2 Skill Relevance Scoring

#### A.2.1 Define relevance scorer `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/relevance.rs`
  - [ ] Implement `rank_skills(message: &str, skills: &[CompressedSkill]) -> Vec<(usize, f64)>`
    - [ ] Extract keywords from user message (reuse `prediction::metrics::extract_keywords`)
    - [ ] Extract keywords from each skill's full_content
    - [ ] Compute Jaccard overlap per skill
    - [ ] Return sorted by relevance descending
    - [ ] Handle CJK: use character bigrams (same as metrics.rs)
  - [ ] Define `RelevanceConfig` struct
    - [ ] `layer1_threshold: f64` (default 0.1 — minimum relevance for Layer 1)
    - [ ] `layer2_threshold: f64` (default 0.4 — minimum relevance for Layer 2)
    - [ ] `max_layer1_skills: usize` (default 5)
    - [ ] `max_layer2_skills: usize` (default 2)

### A.3 Progressive System Prompt Builder

#### A.3.1 Modify build_system_prompt `[MOD]`

- [ ] Modify `channel_reply.rs` `build_system_prompt()`
  - [ ] Add parameter: `compressed_skills: &[CompressedSkill]`
  - [ ] Add parameter: `user_message: &str`
  - [ ] Add parameter: `active_skills: &HashSet<String>` (from Phase B, empty initially)
  - [ ] Build prompt in layers:
    - [ ] SOUL.md (always full)
    - [ ] IDENTITY.md (always full)
    - [ ] Layer 0: all skill names as comma-separated list
    - [ ] Layer 1: top-N relevant skills' summaries (filtered by relevance > 0.1)
    - [ ] Layer 2: active skills OR top-1 highly relevant (relevance > 0.4) — full content
  - [ ] Respect token budget: stop adding layers when budget exhausted
  - [ ] Add token budget tracking to `ReplyContext` or `EvolutionConfig`
- [ ] Update all callers of `build_system_prompt()` to pass new params
- [ ] Add `skill_token_budget: u32` field to `EvolutionConfig` (default 2500)

#### A.3.2 Wire compression into ReplyContext `[MOD]`

- [ ] Add `compressed_skills: Arc<Mutex<CompressedSkillCache>>` to `ReplyContext`
- [ ] Initialize in `server.rs` at startup
- [ ] Refresh cache when registry syncs (every 5 minutes via heartbeat)

### A.4 Phase A Tests

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/tests.rs`
- [ ] Test `CompressedSkill::compress()`:
  - [ ] Skill with YAML description → summary from description
  - [ ] Skill without description → summary from first 2 lines
  - [ ] Token estimates are reasonable (> 0)
- [ ] Test `rank_skills()`:
  - [ ] Message about "rust" → rust_expert ranks first
  - [ ] CJK message → CJK bigram matching works
  - [ ] Empty message → all scores 0
  - [ ] No skills → empty result
- [ ] Test progressive prompt builder:
  - [ ] 0 skills → only SOUL.md
  - [ ] 5 skills, low relevance → Layer 0 + Layer 1 for top matches
  - [ ] 1 highly relevant skill → Layer 2 loaded
  - [ ] Token budget exceeded → stops adding layers

---

## Phase B: Feedback-Driven Activation (Precision)

> Goal: Use PredictionEngine errors to diagnose what skills are needed,
> dynamically activate/deactivate, and detect skill gaps for evolution.

### B.1 Error Diagnostician

#### B.1.1 Define diagnosis types `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/diagnostician.rs`
  - [ ] Define `ErrorCause` enum
    - [ ] `StyleMismatch { aspect: String }` — response too long/short/formal/casual
    - [ ] `DomainGap { topic: String }` — lacks domain knowledge
    - [ ] `PrecisionIssue` — response not accurate enough
    - [ ] `ExpectationMismatch` — user expected different behavior
    - [ ] `Unknown`
  - [ ] Define `ErrorDiagnosis` struct
    - [ ] `primary_cause: ErrorCause`
    - [ ] `related_topics: Vec<String>`
    - [ ] `suggested_skills: Vec<String>` — existing skills that might help
    - [ ] `skill_gap: Option<SkillGap>` — if no existing skill matches
  - [ ] Define `SkillGap` struct
    - [ ] `suggested_name: String`
    - [ ] `suggested_description: String`
    - [ ] `evidence: Vec<String>` — conversation summaries supporting this gap

#### B.1.2 Implement diagnosis logic `[NEW]`

- [ ] In `diagnostician.rs`:
  - [ ] Implement `diagnose(error: &PredictionError, available_skills: &[CompressedSkill]) -> ErrorDiagnosis`
    - [ ] Extract topics from `error.actual.extracted_topics`
    - [ ] If `unexpected_correction` → StyleMismatch or PrecisionIssue
    - [ ] If `unexpected_follow_up` → DomainGap (user keeps asking = agent doesn't know enough)
    - [ ] If `topic_surprise` high → DomainGap (new topic agent hasn't seen)
    - [ ] Match topics against available_skills' keywords:
      - [ ] If match found → `suggested_skills`
      - [ ] If no match → `skill_gap` with suggested name/description
    - [ ] Zero LLM cost — pure rule-based diagnosis

### B.2 Skill Activation Controller

#### B.2.1 Define activation state `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/activation.rs`
  - [ ] Define `ActivationRecord` struct
    - [ ] `skill_name: String`
    - [ ] `agent_id: String`
    - [ ] `activated_at: DateTime<Utc>`
    - [ ] `trigger_error: f64`
    - [ ] `post_activation_errors: RunningStats`
    - [ ] `conversations_while_active: u32`
  - [ ] Define `SkillActivationController` struct
    - [ ] `active_skills: HashMap<String, HashSet<String>>` — agent_id → active skill names
    - [ ] `records: HashMap<(String, String), ActivationRecord>` — (agent_id, skill_name)
    - [ ] `max_active_per_agent: usize` (default 5)

#### B.2.2 Implement activation logic `[NEW]`

- [ ] In `activation.rs`:
  - [ ] Implement `activate(&mut self, agent_id: &str, skill_name: &str, trigger_error: f64)`
    - [ ] Add to active_skills set
    - [ ] Create ActivationRecord
    - [ ] If over max_active → deactivate lowest-performing active skill
  - [ ] Implement `deactivate(&mut self, agent_id: &str, skill_name: &str)`
    - [ ] Remove from active_skills
    - [ ] Set deactivated_at on record
  - [ ] Implement `record_conversation(&mut self, agent_id: &str, prediction_error: f64)`
    - [ ] For each active skill: push error to post_activation_errors, increment count
  - [ ] Implement `evaluate_all(&mut self, agent_id: &str) -> Vec<String>` (returns deactivated)
    - [ ] For each active skill with 10+ conversations:
      - [ ] If `post_activation_errors.mean() >= trigger_error - 0.02` → skill not helping, deactivate
    - [ ] Return list of deactivated skill names
  - [ ] Implement `get_active(&self, agent_id: &str) -> HashSet<String>`

#### B.2.3 Activation persistence (SQLite) `[MIG]`

- [ ] Add migration:
  ```sql
  CREATE TABLE IF NOT EXISTS skill_activations (
      agent_id TEXT NOT NULL,
      skill_name TEXT NOT NULL,
      activated_at TEXT NOT NULL,
      deactivated_at TEXT,
      trigger_error REAL NOT NULL,
      post_errors_json TEXT,
      conversations INTEGER DEFAULT 0,
      PRIMARY KEY (agent_id, skill_name)
  );
  ```
- [ ] Load active skills on startup
- [ ] Persist on activation/deactivation

### B.3 Skill Gap → Evolution Feedback

#### B.3.1 Implement gap injection `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/gap.rs`
  - [ ] Implement `inject_skill_gap(gap: &SkillGap, home_dir: &Path, agent_id: &str)`
    - [ ] Write to `feedback.jsonl` as `signal_type: "skill_gap"`
    - [ ] Include suggested_name, suggested_description, evidence
    - [ ] This gets picked up by Meso reflection's external factors collector

### B.4 Wire into channel_reply.rs `[MOD]`

- [ ] After PredictionError is calculated:
  - [ ] If error is Moderate or higher:
    - [ ] Call `diagnostician.diagnose(error, compressed_skills)`
    - [ ] If `suggested_skills` → call `activation.activate()` for each
    - [ ] If `skill_gap` → call `inject_skill_gap()`
  - [ ] After every conversation:
    - [ ] Call `activation.record_conversation(agent_id, error.composite_error)`
  - [ ] Periodically (every 20 conversations):
    - [ ] Call `activation.evaluate_all()` to prune ineffective skills
- [ ] Pass `activation.get_active()` into `build_system_prompt_progressive()`
  - [ ] Active skills always get Layer 2 (full content)

### B.5 Phase B Tests

- [ ] Test `diagnose()`:
  - [ ] unexpected_correction → StyleMismatch or PrecisionIssue
  - [ ] high topic_surprise + matching skill exists → suggested_skills populated
  - [ ] high topic_surprise + no matching skill → skill_gap populated
  - [ ] Negligible error → no diagnosis (empty)
- [ ] Test `SkillActivationController`:
  - [ ] activate → skill in active set
  - [ ] deactivate → skill removed
  - [ ] record_conversation updates RunningStats
  - [ ] evaluate_all with ineffective skill → deactivated
  - [ ] evaluate_all with effective skill → kept
  - [ ] max_active overflow → lowest performer evicted
- [ ] Test `inject_skill_gap`:
  - [ ] Gap written to feedback.jsonl with correct format
  - [ ] Multiple gaps append (don't overwrite)

---

## Phase C: Skill Distillation (Long-Term Convergence)

> Goal: Skills that are consistently effective graduate into SOUL.md,
> reducing long-term token overhead and evolving the agent organically.

### C.1 Skill Lift Tracker

#### C.1.1 Define lift measurement `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/lift.rs`
  - [ ] Define `SkillLiftTracker` struct
    - [ ] `skill_name: String`
    - [ ] `agent_id: String`
    - [ ] `errors_with: RunningStats` — prediction errors when skill is active
    - [ ] `errors_without: RunningStats` — prediction errors when skill is inactive
    - [ ] `load_count: u64` — how many times loaded into prompt
    - [ ] `first_activated: DateTime<Utc>`
  - [ ] Implement `record_with(&mut self, error: f64)` — skill was active this conversation
  - [ ] Implement `record_without(&mut self, error: f64)` — skill was not active
  - [ ] Implement `lift(&self) -> f64` — `errors_without.mean() - errors_with.mean()` (positive = helps)
  - [ ] Implement `is_stable(&self) -> bool` — std_dev of last 20 errors < 0.1
  - [ ] Implement `is_mature(&self) -> bool` — load_count >= 50 && samples >= 10

#### C.1.2 Wire lift tracking into conversation flow `[MOD]`

- [ ] In `channel_reply.rs` after each conversation:
  - [ ] For each skill in `compressed_skills`:
    - [ ] If skill is active: `lift_tracker.record_with(error.composite_error)`
    - [ ] If skill is not active: `lift_tracker.record_without(error.composite_error)`
  - [ ] Increment `load_count` for active skills

### C.2 Distillation Trigger

#### C.2.1 Define distillation candidate `[NEW]`

- [ ] Create `crates/duduclaw-gateway/src/skill_lifecycle/distillation.rs`
  - [ ] Define `DistillationCandidate` struct
    - [ ] `skill_name: String`
    - [ ] `agent_id: String`
    - [ ] `load_count: u64`
    - [ ] `lift: f64`
    - [ ] `is_stable: bool`
    - [ ] `readiness: f64`
  - [ ] Implement `calculate_readiness(&mut self)`
    - [ ] `usage_maturity = (load_count / 50.0).min(1.0)`
    - [ ] `positive_lift = if lift > 0.05 { 1.0 } else { (lift / 0.05).max(0.0) }`
    - [ ] `stability = if is_stable { 1.0 } else { 0.3 }`
    - [ ] `readiness = 0.3 * usage_maturity + 0.5 * positive_lift + 0.2 * stability`
  - [ ] Define `DISTILLATION_THRESHOLD: f64 = 0.75`

#### C.2.2 Implement distillation scanner `[NEW]`

- [ ] In `distillation.rs`:
  - [ ] Implement `scan_for_distillation(agent_id: &str, trackers: &HashMap<String, SkillLiftTracker>) -> Vec<DistillationCandidate>`
    - [ ] For each mature tracker: build DistillationCandidate, calculate readiness
    - [ ] Return candidates with readiness >= DISTILLATION_THRESHOLD

### C.3 Distillation via GVU Loop

#### C.3.1 Build distillation proposal `[NEW]`

- [ ] In `distillation.rs`:
  - [ ] Implement `build_distillation_input(skill: &CompressedSkill, stats: &DistillationCandidate, soul: &str) -> GeneratorInput`
    - [ ] trigger_context describes the distillation request
    - [ ] Includes skill content in XML isolation tags
    - [ ] Instructs LLM to distill principles (not copy verbatim)
    - [ ] Limit addition to SOUL.md: 2-5 lines maximum

#### C.3.2 Wire distillation into heartbeat/macro `[MOD]`

- [ ] In heartbeat check (or Macro reflection interval):
  - [ ] Call `scan_for_distillation()` for each agent
  - [ ] For each ready candidate:
    - [ ] Call `GvuLoop::run()` with distillation input
    - [ ] On success (Applied):
      - [ ] Archive the skill file (SKILLS/ → SKILLS/archive/)
      - [ ] Remove from active_skills
      - [ ] Log distillation event
    - [ ] On failure (Abandoned/Skipped):
      - [ ] Mark candidate as "attempted", retry after 7 days

### C.4 Phase C Tests

- [ ] Test `SkillLiftTracker`:
  - [ ] lift() with skill helping → positive value
  - [ ] lift() with skill not helping → zero or negative
  - [ ] is_mature() requires 50 loads + 10 samples
  - [ ] is_stable() with low variance → true
- [ ] Test `DistillationCandidate`:
  - [ ] High usage + high lift + stable → readiness > 0.75
  - [ ] Low usage → readiness < 0.75
  - [ ] Negative lift → readiness near 0
- [ ] Test `scan_for_distillation`:
  - [ ] Returns only candidates above threshold
  - [ ] Immature trackers excluded
- [ ] Test `build_distillation_input`:
  - [ ] Trigger context contains skill content in XML tags
  - [ ] Instructions limit to 2-5 lines

---

## Phase D: Integration + Configuration

### D.1 Configuration

- [ ] Add to `EvolutionConfig`:
  ```toml
  [evolution.skills]
  skill_token_budget = 2500
  max_active_skills = 5
  distillation_threshold = 0.75
  distillation_min_loads = 50
  ```

### D.2 Dashboard (Future)

- [ ] Skill lifecycle dashboard page:
  - [ ] Active skills with fitness/lift scores
  - [ ] Skill activation history timeline
  - [ ] Distillation candidates with readiness bars
  - [ ] Skill gaps detected and pending

---

## Summary

| Phase | New Files | Modified Files | New Lines | Tests |
|:-----:|:---------:|:--------------:|:---------:|:-----:|
| A     | 3         | 3              | ~830      | ~250  |
| B     | 3         | 2              | ~900      | ~300  |
| C     | 2         | 2              | ~600      | ~250  |
| D     | 0         | 2              | ~50       | —     |
| **Total** | **8** | **9**       | **~2,380**| **~800** |
