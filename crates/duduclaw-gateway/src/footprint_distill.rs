//! P4-4: digital-footprint memory distillation.
//!
//! Turns the OS-perception event stream (`os_file` / `os_frontmost`, wired by
//! `os_events.rs` / `os_frontmost.rs`) into **temporal memory**, so an agent
//! accumulates a durable, low-noise picture of how its user works — which
//! apps dominate the day, which project directories are active, when the user
//! tends to be active — instead of either ignoring the signal or writing a
//! memory row per raw event (which would flood the store: hundreds of
//! file-saves and window switches happen in a normal working day).
//!
//! Methodology: *Memory for Autonomous LLM Agents* (arXiv:2603.07670) —
//! "write the digital footprint into the existing temporal store, let
//! Ebbinghaus retrievability (`R = exp(-t/S)`) surface what's actually being
//! recalled" — not a new memory subsystem (§②-5 of
//! `research-os-native-agent-methodology.md`: OS-Copilot's declarative/
//! procedural split says facts belong in the existing three-way memory model,
//! never a perception-specific table).
//!
//! ## Design: aggregate in memory, distill once a day
//!
//! [`FootprintTracker`] subscribes to the same autopilot broadcast bus the
//! interruptibility tracker and CEP matcher read, and accumulates **per
//! agent, per UTC day**:
//! - foreground-app *time* (seconds each app was frontmost, accumulated on
//!   each app switch — the interval between two `os_frontmost` events),
//! - active *directory* counts (the containing directory of each `os_file`
//!   path — **never the file name**, data minimization),
//! - an *hour-of-day* activity histogram (UTC).
//!
//! A background ticker ([`FootprintTracker::spawn_distill_loop`]) checks every
//! [`DISTILL_CHECK_INTERVAL`] whether a tracked agent's accumulated day is
//! before "today" (UTC). When it is, the finalized day is **distilled** into
//! up to three deterministic `(subject="user", predicate, object)` triples and
//! written via [`SqliteMemoryEngine::store_temporal`] — never per-event, so
//! the write rate is O(agents × days), not O(events). The existing
//! `(subject, predicate)` supersession chain means today's `daily_active_app`
//! fact automatically closes out yesterday's while `get_history` still exposes
//! the full trail (`R=exp(-t/S)` then ranks by how often that trail keeps
//! getting reaffirmed/searched, per the Ebbinghaus retrievability already in
//! `duduclaw-memory`).
//!
//! The ticker starting at gateway boot (rather than a cron-style fixed
//! midnight trigger) is deliberately what "UTC 日界或 gateway 啟動補跑" means
//! here: there is no persisted backlog to catch up on partial days from a
//! *previous* run (state is pure in-memory, see below) — the loop simply
//! keeps checking date rollover regardless of what time of day the gateway
//! happened to start, so a long-running process naturally distills every UTC
//! midnight it lives through.
//!
//! ## Opt-in, additive, in-memory only
//!
//! - Per-agent opt-in: `[os_watch] footprint = true` (deny-by-default, layered
//!   ON TOP of `os_native` / `[os_watch] paths` — an agent with filesystem
//!   watching on does NOT get footprint memory unless it also sets this flag).
//!   Read via [`read_footprint_enabled`]; only agents that had it set **at
//!   gateway startup** are tracked — no hot-reload (same documented tradeoff
//!   as `os_frontmost`'s `frontmost_poll_secs` polling, P2-4: "a future pass
//!   can add a stoppable registry symmetric with `OsWatcherRegistry`"). This
//!   is also the aggregation-side half of deny-by-default: an agent that never
//!   opted in is never even added to the in-memory tracking map, not merely
//!   skipped at write time — data minimization starts at collection, not just
//!   at persistence.
//! - Aggregation state is pure in-memory (`std::sync::Mutex` over a `HashMap`,
//!   never held across an `.await`) — a gateway restart loses the current UTC
//!   day's partial stats (same tradeoff as the P3-3 CEP pending window / P3-4
//!   kickoff debounce state: documented, not silent, and low-stakes here since
//!   at most one day of aggregation is ever at risk).
//! - Every piece of perceived text (app name, directory path) is passed
//!   through `sanitize_perception_text` before it is ever used as an
//!   aggregation key or written to memory — the same P2-5 perception boundary
//!   every other OS-sensing path uses.
//! - File paths are reduced to their **containing directory** before anything
//!   else happens to them — [`directory_of`] runs on the raw path, so the file
//!   name itself is never even looked at, let alone aggregated or stored
//!   (stronger than sanitizing-then-truncating a full path: the highest-risk
//!   substring — the file name, "quarterly-layoffs-draft.docx" — is dropped
//!   at the source, not merely neutralized).
//!
//! ## Origin + sensitivity (v1.41 write-time binding, P3-2)
//!
//! Every write is stamped `origin = "agent_derived"` (trust ceiling `0.6`,
//! `duduclaw_memory::origin::AGENT_DERIVED`) — this is background-pipeline
//! *derived* content (a deterministic aggregation over raw OS signals), not
//! something the user typed (`user_direct`, ceiling `1.0`) nor a raw tool
//! echo (`tool_echo`, ceiling `0.5`); it is the same shape as reflexion
//! consolidation / night consolidation, which is exactly what
//! `AGENT_DERIVED`'s doc comment lists as its intended callers.
//!
//! Each written row also carries a [`Sensitivity`] label in `metadata` via
//! `duduclaw_memory::stamp_sensitivity_metadata` — the P3-2 write-side hook
//! this module is the first production caller of:
//! - `daily_active_app` / `active_hours` are identity-bound behavioral
//!   signals → [`Sensitivity::Personal`] (mirrors the P3-2 perception-source
//!   table's `frontmost` = Personal rule — both ultimately derive from the
//!   same `os_frontmost` sensing source; `active_hours` reveals *when* a
//!   specific person works, which is the same category of exposure as a
//!   calendar, also classified Personal).
//! - `active_directory` is workspace/operational → [`Sensitivity::Internal`]
//!   (mirrors the P3-2 table's `os_file` = Internal rule).
//!
//! **Known limitation, stated precisely:** no consumer currently reads
//! memory-row `sensitivity` metadata to gate `memory_search` / `search_layer`
//! results by session privacy — P3-2 shipped its context-collapse defence at
//! the persona-block (`## Key Facts About This User`, `## About This User`)
//! and wiki-namespace layers only, both of which gate on `is_private_session`
//! *before* ever querying memory, not by filtering already-returned rows. This
//! module supplies the correctly-labelled write side that such a future
//! retrieval-side gate can key off (`duduclaw_memory::read_sensitivity_metadata`
//! + `Sensitivity::allowed_in_session`); the accompanying test proves that
//! composition is correct for these three predicates specifically, without
//! claiming a `channel_reply.rs`-level consumer exists yet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Timelike, Utc};
use tokio::sync::{RwLock, broadcast};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::Sensitivity;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};
use duduclaw_security::perception::{DEFAULT_PERCEPTION_MAX_CHARS, sanitize_perception_text};

use crate::autopilot_engine::AutopilotEvent;

/// `subject` value for every footprint triple — the desktop's single OS user,
/// not a specific chat-channel identity. (The `user:<id>` convention in
/// `duduclaw_memory::user_profile` is for per-chat-user preference facts; OS
/// perception has no channel identity to key on — there is exactly one person
/// sitting at the keyboard the watcher observes.)
pub const FOOTPRINT_SUBJECT: &str = "user";

/// Top-N frontmost apps by accumulated foreground seconds, UTC day.
pub const PREDICATE_DAILY_ACTIVE_APP: &str = "daily_active_app";
/// Top-N containing directories by `os_file` event count, UTC day.
pub const PREDICATE_ACTIVE_DIRECTORY: &str = "active_directory";
/// Top-N most-active UTC hours by combined `os_file` + `os_frontmost` event count.
pub const PREDICATE_ACTIVE_HOURS: &str = "active_hours";

/// `source_event` stamped on every write — lets a future rollback
/// (`origin_purge`) or dedup scan target exactly these rows, same convention
/// as `wiki_ingest::DISTILL_SOURCE_EVENT` / `user_profile`'s `"user_profile"`.
pub const FOOTPRINT_SOURCE_EVENT: &str = "footprint_distill";

const FOOTPRINT_TAG: &str = "footprint-distill";

/// v1.41 origin binding (WP1 table, `duduclaw_memory::origin`): background-
/// pipeline derived content, not user-typed or a raw tool echo.
const FOOTPRINT_ORIGIN: &str = duduclaw_memory::origin::AGENT_DERIVED.name;
/// Declared trust equals the class ceiling exactly (same pattern as
/// `wiki_ingest::DISTILL_ORIGIN_TRUST`) — `store_temporal` would clamp a
/// higher declared value down to this anyway; being explicit documents intent.
const FOOTPRINT_ORIGIN_TRUST: f64 = duduclaw_memory::origin::AGENT_DERIVED.ceiling;

/// Deterministic, computed statistics about what actually happened — not an
/// inference — so importance sits above the reflexion-rule default (5.0 is
/// the engine default; this is intentionally a notch below explicit user
/// preferences like `user_profile`'s 6.0, and a notch above wiki-distilled
/// chat facts' 5.0, reflecting "definitely happened" but "background/
/// low-salience" content).
const FOOTPRINT_IMPORTANCE: f64 = 4.0;

const TOP_N_APPS: usize = 5;
const TOP_N_DIRS: usize = 5;
const TOP_N_HOURS: usize = 4;

/// How often the distill ticker checks tracked agents for a crossed UTC day
/// boundary. Short enough that "yesterday's" facts land in memory well before
/// the next conversation is likely to want them; long enough not to matter for
/// gateway load (a HashMap scan + at most a handful of DB writes).
const DISTILL_CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Read `[os_watch] footprint` from an agent's `agent.toml`.
///
/// Additive raw-TOML parse — same pattern as `os_events::read_os_watch_config`
/// / `os_frontmost::read_frontmost_poll_secs` — never touches the serde
/// `AgentConfig` struct, so this new key can't break existing configs.
/// **Deny-by-default**: absent file, absent `[os_watch]` table, absent key,
/// malformed TOML, or a non-boolean value all resolve to `false` — digital-
/// footprint aggregation is opt-in on top of `os_native` / `[os_watch]`, never
/// implied by them.
pub fn read_footprint_enabled(agent_dir: &Path) -> bool {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(?path, error = %e, "malformed agent.toml — [os_watch] footprint ignored");
            return false;
        }
    };
    value
        .get("os_watch")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("footprint"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Reduce a perceived `os_file` path to its containing directory. Runs on the
/// **raw** path before any sanitization, so the file name is dropped before
/// it is ever inspected, aggregated, or stored — the strongest form of data
/// minimization (nothing downstream can leak a substring of the file name if
/// nothing downstream ever holds it).
///
/// Falls back to the input unchanged only when there is no parent component
/// at all (e.g. a bare relative name with no separator) — in practice `os_file`
/// paths from the watcher are always absolute, so this fallback is untaken in
/// production; kept as a defined behavior rather than a panic for a malformed
/// out-of-process `events.db` writer.
pub fn directory_of(path: &str) -> String {
    let p = Path::new(path);
    match p.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

/// Render `total_secs` as a compact human string (`"3h12m"` / `"42m"`).
fn format_duration_secs(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}

/// One agent's in-progress UTC-day aggregation. Reset (not mutated in place)
/// on distillation — [`FootprintTracker::distill_and_reset`] swaps in a fresh
/// instance rather than clearing this one, so a concurrent `note_event` can
/// never observe a half-cleared struct.
#[derive(Debug, Clone)]
struct AgentDayStats {
    /// The UTC calendar day this bucket is accumulating for.
    date: NaiveDate,
    /// Sanitized app name → accumulated foreground seconds.
    app_seconds: HashMap<String, u64>,
    /// The app currently in the foreground and when it became so, if any
    /// `os_frontmost` event has been observed since the bucket started.
    /// Folded into `app_seconds` (and carried forward into the next bucket)
    /// at distillation time, so an app left open across midnight doesn't lose
    /// the portion of its duration up to the flush point.
    current_app: Option<(String, DateTime<Utc>)>,
    /// Sanitized containing-directory → `os_file` event count.
    dir_counts: HashMap<String, u64>,
    /// UTC hour-of-day (0..24) → combined `os_file` + `os_frontmost` event count.
    hour_counts: [u64; 24],
}

impl AgentDayStats {
    fn new(date: NaiveDate) -> Self {
        Self {
            date,
            app_seconds: HashMap::new(),
            current_app: None,
            dir_counts: HashMap::new(),
            hour_counts: [0; 24],
        }
    }

    /// True once `current_app` has been folded (distillation does this before
    /// finalizing) — i.e. nothing at all happened in this bucket.
    fn is_empty(&self) -> bool {
        self.app_seconds.is_empty()
            && self.current_app.is_none()
            && self.dir_counts.is_empty()
            && self.hour_counts.iter().all(|c| *c == 0)
    }
}

/// One rendered footprint triple, ready for `store_temporal`:
/// `(predicate, human-readable content, compact object encoding, sensitivity)`.
type FootprintTriple = (&'static str, String, String, Sensitivity);

fn render_daily_active_app(stats: &AgentDayStats) -> Option<FootprintTriple> {
    let mut apps: Vec<(&String, &u64)> =
        stats.app_seconds.iter().filter(|(_, s)| **s > 0).collect();
    if apps.is_empty() {
        return None;
    }
    // Deterministic order: seconds descending, app name ascending as tiebreak.
    apps.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    apps.truncate(TOP_N_APPS);
    let object = apps
        .iter()
        .map(|(a, s)| format!("{a}={s}s"))
        .collect::<Vec<_>>()
        .join(";");
    let human = apps
        .iter()
        .map(|(a, s)| format!("{a}（{}）", format_duration_secs(**s)))
        .collect::<Vec<_>>()
        .join("、");
    let content = format!(
        "{} 前景應用程式使用時長 Top{}（UTC）：{human}",
        stats.date,
        apps.len()
    );
    Some((
        PREDICATE_DAILY_ACTIVE_APP,
        content,
        object,
        Sensitivity::Personal,
    ))
}

fn render_active_directory(stats: &AgentDayStats) -> Option<FootprintTriple> {
    if stats.dir_counts.is_empty() {
        return None;
    }
    let mut dirs: Vec<(&String, &u64)> = stats.dir_counts.iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    dirs.truncate(TOP_N_DIRS);
    let object = dirs
        .iter()
        .map(|(d, c)| format!("{d}={c}"))
        .collect::<Vec<_>>()
        .join(";");
    let human = dirs
        .iter()
        .map(|(d, c)| format!("{d}（{c}次）"))
        .collect::<Vec<_>>()
        .join("、");
    let content = format!(
        "{} 活躍目錄 Top{}（依檔案事件次數，目錄層級，不含檔名）：{human}",
        stats.date,
        dirs.len()
    );
    Some((
        PREDICATE_ACTIVE_DIRECTORY,
        content,
        object,
        Sensitivity::Internal,
    ))
}

fn render_active_hours(stats: &AgentDayStats) -> Option<FootprintTriple> {
    let mut hours: Vec<(usize, u64)> = stats
        .hour_counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(h, c)| (h, *c))
        .collect();
    if hours.is_empty() {
        return None;
    }
    // Rank by count to pick the top-N, then re-sort ascending by hour for a
    // readable "timeline" rendering.
    hours.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hours.truncate(TOP_N_HOURS);
    hours.sort_by_key(|(h, _)| *h);
    let object = hours
        .iter()
        .map(|(h, c)| format!("{h:02}={c}"))
        .collect::<Vec<_>>()
        .join(";");
    let human = hours
        .iter()
        .map(|(h, _)| format!("{h:02}:00"))
        .collect::<Vec<_>>()
        .join("、");
    let content = format!(
        "{} 活躍時段 Top{}（UTC 小時）：{human}",
        stats.date,
        hours.len()
    );
    Some((
        PREDICATE_ACTIVE_HOURS,
        content,
        object,
        Sensitivity::Personal,
    ))
}

/// Render every non-empty predicate for `stats`. Empty when the day had no
/// observed activity at all (nothing is written in that case).
fn render_triples(stats: &AgentDayStats) -> Vec<FootprintTriple> {
    [
        render_daily_active_app(stats),
        render_active_directory(stats),
        render_active_hours(stats),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Write the rendered footprint triples for `stats` into `engine` for
/// `agent_id`. Returns the ids written (empty when the day had no activity).
///
/// Shared by the production path ([`FootprintTracker::distill_and_reset`],
/// invoked inside `spawn_blocking` — `SqliteMemoryEngine` wraps rusqlite and
/// the project convention is to keep blocking DB work off the async runtime
/// thread, mirrored from `autopilot_engine::fetch_persona_lines`) and
/// directly by tests (calling `SqliteMemoryEngine` methods `.await` from a
/// `#[tokio::test]` body needs no `spawn_blocking` — the engine's own test
/// suite does the same).
async fn write_footprint_triples(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    stats: &AgentDayStats,
) -> duduclaw_core::error::Result<Vec<String>> {
    let mut ids = Vec::new();
    for (predicate, content, object, sensitivity) in render_triples(stats) {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content,
            timestamp: Utc::now(),
            tags: vec![FOOTPRINT_TAG.to_string()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: FOOTPRINT_IMPORTANCE,
            access_count: 0,
            last_accessed: None,
            source_event: FOOTPRINT_SOURCE_EVENT.to_string(),
        };
        let meta = TemporalMeta {
            subject: Some(FOOTPRINT_SUBJECT.to_string()),
            predicate: Some(predicate.to_string()),
            object: Some(object),
            origin: Some(FOOTPRINT_ORIGIN.to_string()),
            origin_trust: Some(FOOTPRINT_ORIGIN_TRUST),
            metadata: Some(duduclaw_memory::stamp_sensitivity_metadata(
                None,
                sensitivity,
            )),
            ..TemporalMeta::default()
        };
        let id = engine.store_temporal(agent_id, entry, meta).await?;
        ids.push(id);
    }
    Ok(ids)
}

/// Tracks per-agent digital-footprint aggregation from the autopilot sensing
/// broadcast and periodically distills it into temporal memory.
///
/// Interior-mutable (`std::sync::Mutex` over an in-memory map — never held
/// across an `.await`, same convention as `InterruptibilityTracker`) so it can
/// be shared as `Arc<FootprintTracker>` between the ingest task and the
/// distill ticker.
pub struct FootprintTracker {
    home_dir: PathBuf,
    /// Agent ids (directory names) with `[os_watch] footprint = true`.
    /// Deny-by-default at the aggregation layer, not just the write layer — an
    /// agent not in this set is never even added to `state`. **Interior-mutable
    /// (P4-3)**: `os.settings.update` toggles membership via [`set_enabled`] so
    /// a footprint edit takes effect without a gateway restart — the singleton
    /// ingest task re-checks membership per event and the distill loop reads a
    /// fresh snapshot each tick, so no per-agent task churn is needed.
    ///
    /// [`set_enabled`]: FootprintTracker::set_enabled
    enabled_agents: std::sync::Mutex<HashSet<String>>,
    state: Mutex<HashMap<String, AgentDayStats>>,
}

impl FootprintTracker {
    pub fn new(home_dir: PathBuf, enabled_agents: HashSet<String>) -> Arc<Self> {
        Arc::new(Self {
            home_dir,
            enabled_agents: std::sync::Mutex::new(enabled_agents),
            state: Mutex::new(HashMap::new()),
        })
    }

    /// Number of agents currently opted into footprint aggregation (test /
    /// status helper).
    pub fn enabled_count(&self) -> usize {
        self.enabled_agents.lock().unwrap().len()
    }

    /// Whether one agent is currently opted into footprint aggregation.
    pub fn is_enabled(&self, agent_id: &str) -> bool {
        self.enabled_agents.lock().unwrap().contains(agent_id)
    }

    /// Hot enable/disable footprint aggregation for one agent (P4-3 dashboard
    /// edit). Disabling also drops the agent's in-progress day bucket so a
    /// partial day is neither distilled after opt-out nor carried if it opts
    /// back in — a clean stop. Enabling is a no-op if already enabled.
    pub fn set_enabled(&self, agent_id: &str, on: bool) {
        let mut set = self.enabled_agents.lock().unwrap();
        if on {
            set.insert(agent_id.to_string());
        } else if set.remove(agent_id) {
            drop(set);
            self.state.lock().unwrap().remove(agent_id);
        }
    }

    fn record_frontmost(&self, agent_id: &str, raw_app: &str, now: DateTime<Utc>) {
        if !self.is_enabled(agent_id) {
            return;
        }
        let app = sanitize_perception_text(raw_app, DEFAULT_PERCEPTION_MAX_CHARS).text;
        let mut map = self.state.lock().unwrap();
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| AgentDayStats::new(now.date_naive()));
        if let Some((prev_app, since)) = entry.current_app.take() {
            let elapsed = (now - since).num_seconds().max(0) as u64;
            *entry.app_seconds.entry(prev_app).or_insert(0) += elapsed;
        }
        entry.current_app = Some((app, now));
        entry.hour_counts[now.hour() as usize] += 1;
    }

    fn record_file_event(&self, agent_id: &str, raw_path: &str, now: DateTime<Utc>) {
        if !self.is_enabled(agent_id) {
            return;
        }
        // Directory-only reduction happens on the RAW path (before
        // sanitization ever sees a file name), then the resulting directory
        // string is itself sanitized before use as an aggregation key —
        // belt-and-braces: the file name is dropped at the source AND
        // whatever remains still goes through the perception boundary.
        let dir_raw = directory_of(raw_path);
        let dir = sanitize_perception_text(&dir_raw, DEFAULT_PERCEPTION_MAX_CHARS).text;
        let mut map = self.state.lock().unwrap();
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| AgentDayStats::new(now.date_naive()));
        *entry.dir_counts.entry(dir).or_insert(0) += 1;
        entry.hour_counts[now.hour() as usize] += 1;
    }

    /// Route one autopilot event into the appropriate aggregation. Non-OS
    /// events are ignored. No-op for any agent not in `enabled_agents`.
    pub fn note_event(&self, event: &AutopilotEvent, now: DateTime<Utc>) {
        match event {
            AutopilotEvent::OsFrontmostEvent { agent_id, app, .. } => {
                self.record_frontmost(agent_id, app, now)
            }
            AutopilotEvent::OsFileEvent { agent_id, path, .. } => {
                self.record_file_event(agent_id, path, now)
            }
            _ => {}
        }
    }

    /// If `agent_id`'s current bucket belongs to a UTC day strictly before
    /// `now`'s day, finalize and swap in a fresh bucket for `now`'s day,
    /// carrying forward any still-open frontmost session. No-op (and no
    /// write) when there is no bucket yet, the bucket is already "today", or
    /// the finalized bucket has no observed activity.
    async fn distill_and_reset(&self, agent_id: &str, now: DateTime<Utc>) {
        let finalized = {
            let mut map = self.state.lock().unwrap();
            let Some(prev) = map.get_mut(agent_id) else {
                return;
            };
            if prev.date >= now.date_naive() {
                return;
            }
            let carried_app = prev.current_app.take().map(|(app, since)| {
                let elapsed = (now - since).num_seconds().max(0) as u64;
                *prev.app_seconds.entry(app.clone()).or_insert(0) += elapsed;
                app
            });
            let mut fresh = AgentDayStats::new(now.date_naive());
            if let Some(app) = carried_app {
                fresh.current_app = Some((app, now));
            }
            std::mem::replace(prev, fresh)
        };

        if finalized.is_empty() {
            debug!(agent = %agent_id, date = %finalized.date, "P4-4 footprint: no activity observed, nothing distilled");
            return;
        }

        let home_dir = self.home_dir.clone();
        let agent_owned = agent_id.to_string();
        let date_str = finalized.date.to_string();
        let res = tokio::task::spawn_blocking(move || {
            let db_path = home_dir.join("memory.db");
            let engine = SqliteMemoryEngine::new(&db_path)?;
            let rt = tokio::runtime::Handle::current();
            rt.block_on(write_footprint_triples(&engine, &agent_owned, &finalized))
        })
        .await;
        match res {
            Ok(Ok(ids)) => {
                info!(
                    agent = %agent_id,
                    date = %date_str,
                    n = ids.len(),
                    "P4-4 footprint distilled into temporal memory"
                );
            }
            Ok(Err(e)) => {
                warn!(agent = %agent_id, date = %date_str, error = %e, "P4-4 footprint distillation store failed");
            }
            Err(e) => {
                warn!(agent = %agent_id, date = %date_str, error = %e, "P4-4 footprint distillation task panicked");
            }
        }
    }

    /// Spawn the background task that feeds `rx` into this tracker. Lagged
    /// events are tolerated (footprint is a soft daily-usage estimate, missing
    /// a few switches only under-counts a day's tally).
    pub fn spawn_ingest(
        self: Arc<Self>,
        mut rx: broadcast::Receiver<AutopilotEvent>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => self.note_event(&event, Utc::now()),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(dropped = n, "P4-4 footprint: broadcast lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        warn!("P4-4 footprint: broadcast closed — ingest task stopping");
                        break;
                    }
                }
            }
        })
    }

    /// Spawn the periodic UTC-day-rollover check + distill loop. Runs for the
    /// process lifetime; see the module docs for why "gateway startup" needs
    /// no special-cased catch-up logic beyond simply running this loop.
    pub fn spawn_distill_loop(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(DISTILL_CHECK_INTERVAL);
            loop {
                ticker.tick().await;
                let now = Utc::now();
                // Fresh snapshot each tick so a hot opt-in/opt-out (P4-3
                // `set_enabled`) is honored without restarting this loop.
                let agents: Vec<String> = self
                    .enabled_agents
                    .lock()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect();
                for agent_id in agents {
                    self.distill_and_reset(&agent_id, now).await;
                }
            }
        })
    }
}

/// Seed the (handler-held) [`FootprintTracker`] from the current agent
/// registry and spawn its two background tasks (ingest + distill ticker).
///
/// Scans for agents with `[capabilities] os_native = true` AND `[os_watch]
/// footprint = true` that are also within the quota-resolved `allowed` set
/// (see `os_events::resolve_os_native_allowed`), and calls
/// [`FootprintTracker::set_enabled`] for each. Unlike the pre-P4-3 version this
/// **always** spawns the two tasks — even when nobody opted in at startup — so
/// a later hot opt-in via `os.settings.update` takes effect without a restart
/// (the ingest task filters by live membership; the distill loop reads a fresh
/// snapshot each tick). Returns the two background `JoinHandle`s.
///
/// `agent_id` keys use the agent's **directory name**, matching the
/// `os_file` / `os_frontmost` event convention so the same identifier is used
/// end-to-end.
pub async fn init_footprint_distill(
    tracker: Arc<FootprintTracker>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    tx: broadcast::Sender<AutopilotEvent>,
    allowed: &HashSet<String>,
) -> Vec<JoinHandle<()>> {
    let enabled: HashSet<String> = {
        let reg = agent_registry.read().await;
        reg.list()
            .iter()
            .filter(|a| a.config.capabilities.os_native)
            .filter_map(|a| {
                let id = a.dir.file_name().and_then(|n| n.to_str())?;
                if !allowed.contains(id) {
                    return None;
                }
                if !read_footprint_enabled(&a.dir) {
                    return None;
                }
                Some(id.to_string())
            })
            .collect()
    };

    for id in &enabled {
        tracker.set_enabled(id, true);
    }
    if enabled.is_empty() {
        info!(
            "no agents configured with [os_watch] footprint = true — digital-footprint distillation idle (tasks still armed for hot opt-in)"
        );
    } else {
        info!(agents = ?enabled, "starting P4-4 digital-footprint distillation");
    }

    let ingest = tracker.clone().spawn_ingest(tx.subscribe());
    let ticker = tracker.spawn_distill_loop();
    vec![ingest, ticker]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn enabled_set(agent: &str) -> HashSet<String> {
        [agent.to_string()].into_iter().collect()
    }

    // ── read_footprint_enabled: config reader ─────────────────────────────

    #[test]
    fn read_footprint_enabled_absent_or_missing_table_is_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!read_footprint_enabled(dir.path()));

        std::fs::write(
            dir.path().join("agent.toml"),
            "[capabilities]\nos_native = true\n",
        )
        .unwrap();
        assert!(!read_footprint_enabled(dir.path()));

        std::fs::write(dir.path().join("agent.toml"), "[os_watch]\npaths = []\n").unwrap();
        assert!(!read_footprint_enabled(dir.path()));
    }

    #[test]
    fn read_footprint_enabled_malformed_toml_is_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "not valid [[[ toml").unwrap();
        assert!(!read_footprint_enabled(dir.path()));
    }

    #[test]
    fn read_footprint_enabled_non_bool_value_is_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\nfootprint = \"yes\"\n",
        )
        .unwrap();
        assert!(!read_footprint_enabled(dir.path()));
    }

    #[test]
    fn read_footprint_enabled_explicit_false_is_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\nfootprint = false\n",
        )
        .unwrap();
        assert!(!read_footprint_enabled(dir.path()));
    }

    #[test]
    fn read_footprint_enabled_true_is_true() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[os_watch]\nfootprint = true\n",
        )
        .unwrap();
        assert!(read_footprint_enabled(dir.path()));
    }

    // ── directory_of: file-name dropped, directory kept ───────────────────

    #[test]
    fn directory_of_drops_file_name() {
        assert_eq!(
            directory_of("/Users/me/proj/src/main.rs"),
            "/Users/me/proj/src"
        );
        assert_eq!(directory_of("/Users/me/secret-plan.docx"), "/Users/me");
    }

    #[test]
    fn directory_of_bare_name_falls_back_unchanged() {
        // No separator at all — parent() yields an empty component; falls
        // back to the input rather than panicking. Documented, not the
        // in-production path (watcher paths are always absolute).
        assert_eq!(directory_of("bare-name"), "bare-name");
    }

    // ── format_duration_secs ───────────────────────────────────────────────

    #[test]
    fn format_duration_secs_formats_hours_and_minutes() {
        assert_eq!(format_duration_secs(42 * 60), "42m");
        assert_eq!(format_duration_secs(3 * 3600 + 12 * 60), "3h12m");
        assert_eq!(format_duration_secs(0), "0m");
    }

    // ── aggregation: deny-by-default, sanitization, directory-only ────────

    #[test]
    fn disabled_agent_events_are_never_aggregated() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = FootprintTracker::new(dir.path().to_path_buf(), HashSet::new());
        tracker.note_event(
            &AutopilotEvent::OsFrontmostEvent {
                agent_id: "agent-x".into(),
                app: "VSCode".into(),
                window_title: "".into(),
                prev_app: "".into(),
            },
            utc(2026, 7, 20, 10, 0),
        );
        tracker.note_event(
            &AutopilotEvent::OsFileEvent {
                agent_id: "agent-x".into(),
                path: "/Users/me/proj/main.rs".into(),
                change: "modified".into(),
            },
            utc(2026, 7, 20, 10, 0),
        );
        let map = tracker.state.lock().unwrap();
        assert!(
            map.is_empty(),
            "an agent without [os_watch] footprint=true must never be aggregated"
        );
    }

    #[test]
    fn set_enabled_hot_toggles_membership_and_drops_bucket_on_disable() {
        let dir = tempfile::tempdir().unwrap();
        // Start with nobody opted in (the always-armed P4-3 startup shape).
        let tracker = FootprintTracker::new(dir.path().to_path_buf(), HashSet::new());
        assert!(!tracker.is_enabled("agent-x"));

        // Hot enable → events now aggregate.
        tracker.set_enabled("agent-x", true);
        assert!(tracker.is_enabled("agent-x"));
        assert_eq!(tracker.enabled_count(), 1);
        tracker.record_file_event("agent-x", "/Users/me/proj/a.rs", utc(2026, 7, 20, 9, 0));
        assert!(tracker.state.lock().unwrap().contains_key("agent-x"));

        // Hot disable → membership cleared AND the partial-day bucket dropped.
        tracker.set_enabled("agent-x", false);
        assert!(!tracker.is_enabled("agent-x"));
        assert_eq!(tracker.enabled_count(), 0);
        assert!(
            tracker.state.lock().unwrap().is_empty(),
            "disabling must drop the in-progress bucket so no partial day is distilled after opt-out"
        );

        // Post-disable events are ignored again.
        tracker.record_file_event("agent-x", "/Users/me/proj/b.rs", utc(2026, 7, 20, 10, 0));
        assert!(tracker.state.lock().unwrap().is_empty());
    }

    #[test]
    fn record_file_event_never_stores_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = FootprintTracker::new(dir.path().to_path_buf(), enabled_set("agent-x"));
        tracker.record_file_event(
            "agent-x",
            "/Users/me/very-secret-plan.docx",
            utc(2026, 7, 20, 9, 0),
        );
        let map = tracker.state.lock().unwrap();
        let stats = map.get("agent-x").unwrap();
        assert_eq!(stats.dir_counts.len(), 1);
        let key = stats.dir_counts.keys().next().unwrap();
        assert_eq!(key, "/Users/me");
        assert!(!key.contains("secret-plan"));
        assert_eq!(stats.hour_counts[9], 1);
    }

    #[test]
    fn record_frontmost_accumulates_seconds_between_switches() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = FootprintTracker::new(dir.path().to_path_buf(), enabled_set("agent-x"));
        tracker.record_frontmost("agent-x", "VSCode", utc(2026, 7, 20, 9, 0));
        // 30 minutes later, switch to Chrome — closes out VSCode's interval.
        tracker.record_frontmost("agent-x", "Chrome", utc(2026, 7, 20, 9, 30));
        let map = tracker.state.lock().unwrap();
        let stats = map.get("agent-x").unwrap();
        assert_eq!(stats.app_seconds.get("VSCode"), Some(&1800));
        // Chrome is still open (no closing switch yet) — not yet in app_seconds.
        assert!(!stats.app_seconds.contains_key("Chrome"));
        assert_eq!(stats.current_app.as_ref().unwrap().0, "Chrome");
    }

    #[test]
    fn note_event_ignores_non_os_events() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = FootprintTracker::new(dir.path().to_path_buf(), enabled_set("agent-x"));
        tracker.note_event(
            &AutopilotEvent::AgentIdle {
                agent_id: "agent-x".into(),
                idle_minutes: 5,
            },
            utc(2026, 7, 20, 9, 0),
        );
        let map = tracker.state.lock().unwrap();
        assert!(map.is_empty());
    }

    // ── render_triples: pure rendering ─────────────────────────────────────

    #[test]
    fn render_triples_empty_stats_yields_nothing() {
        let stats = AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
        assert!(render_triples(&stats).is_empty());
    }

    #[test]
    fn render_daily_active_app_sorts_by_seconds_desc_top_n() {
        let mut stats = AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
        stats.app_seconds.insert("A".into(), 100);
        stats.app_seconds.insert("B".into(), 300);
        stats.app_seconds.insert("C".into(), 200);
        let (predicate, content, object, sensitivity) = render_daily_active_app(&stats).unwrap();
        assert_eq!(predicate, PREDICATE_DAILY_ACTIVE_APP);
        assert_eq!(sensitivity, Sensitivity::Personal);
        assert_eq!(object, "B=300s;C=200s;A=100s");
        assert!(content.contains("2026-07-20"));
    }

    #[test]
    fn render_active_directory_is_internal_sensitivity() {
        let mut stats = AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
        stats.dir_counts.insert("/Users/me/proj".into(), 4);
        let (predicate, _content, object, sensitivity) = render_active_directory(&stats).unwrap();
        assert_eq!(predicate, PREDICATE_ACTIVE_DIRECTORY);
        assert_eq!(sensitivity, Sensitivity::Internal);
        assert_eq!(object, "/Users/me/proj=4");
    }

    #[test]
    fn render_active_hours_top_n_then_ascending_by_hour() {
        let mut stats = AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
        stats.hour_counts[22] = 1;
        stats.hour_counts[9] = 5;
        stats.hour_counts[14] = 3;
        let (predicate, _content, object, sensitivity) = render_active_hours(&stats).unwrap();
        assert_eq!(predicate, PREDICATE_ACTIVE_HOURS);
        assert_eq!(sensitivity, Sensitivity::Personal);
        // Ascending by hour in the rendered object, even though selection was by count.
        assert_eq!(object, "09=5;14=3;22=1");
    }

    // ── write_footprint_triples: origin + sensitivity stamping ────────────

    #[tokio::test]
    async fn write_footprint_triples_stamps_origin_and_sensitivity() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut stats = AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
        stats.app_seconds.insert("VSCode".into(), 3600);
        stats.dir_counts.insert("/Users/me/proj".into(), 4);
        stats.hour_counts[9] = 3;

        let ids = write_footprint_triples(&engine, "agent-x", &stats)
            .await
            .unwrap();
        assert_eq!(ids.len(), 3);

        for id in &ids {
            let trust = engine.get_origin_trust("agent-x", id).await.unwrap();
            assert_eq!(
                trust,
                Some(duduclaw_memory::origin::AGENT_DERIVED.ceiling),
                "every footprint write must be clamped to the agent_derived ceiling"
            );
        }

        let facts = engine
            .list_valid_by_source_event("agent-x", FOOTPRINT_SOURCE_EVENT, 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 3);

        let app_fact = facts
            .iter()
            .find(|(e, _)| e.content.contains("VSCode"))
            .expect("daily_active_app fact present");
        assert_eq!(
            duduclaw_memory::read_sensitivity_metadata(Some(&app_fact.1)),
            Sensitivity::Personal
        );

        let dir_fact = facts
            .iter()
            .find(|(e, _)| e.content.contains("/Users/me/proj"))
            .expect("active_directory fact present");
        assert_eq!(
            duduclaw_memory::read_sensitivity_metadata(Some(&dir_fact.1)),
            Sensitivity::Internal
        );

        let hours_fact = facts
            .iter()
            .find(|(e, _)| e.content.contains("活躍時段"))
            .expect("active_hours fact present");
        assert_eq!(
            duduclaw_memory::read_sensitivity_metadata(Some(&hours_fact.1)),
            Sensitivity::Personal
        );
    }

    #[tokio::test]
    async fn write_footprint_triples_no_activity_writes_nothing() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let stats = AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
        let ids = write_footprint_triples(&engine, "agent-x", &stats)
            .await
            .unwrap();
        assert!(ids.is_empty());
    }

    /// Proves the write-time [`Sensitivity`] labelling composes correctly with
    /// the existing P3-2 context-collapse primitive
    /// (`Sensitivity::allowed_in_session`): a Personal-labelled footprint fact
    /// is withheld from a group/shared session while an Internal-labelled one
    /// remains visible, and a private 1:1 session sees everything. This is the
    /// retrieval-side guarantee the module doc's "known limitation" section
    /// describes — no `channel_reply.rs` consumer wires this yet, but the
    /// write side is proven ready for one.
    #[tokio::test]
    async fn group_chat_would_strip_personal_predicates_but_keep_directory() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let mut stats = AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
        stats.app_seconds.insert("VSCode".into(), 1000);
        stats.dir_counts.insert("/Users/me/proj".into(), 2);
        stats.hour_counts[9] = 1;
        write_footprint_triples(&engine, "agent-x", &stats)
            .await
            .unwrap();

        let facts = engine
            .list_valid_by_source_event("agent-x", FOOTPRINT_SOURCE_EVENT, 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 3);

        let is_group = false;
        let is_dm = true;
        let mut checked_personal = 0;
        let mut checked_internal = 0;
        for (entry, meta) in &facts {
            let sensitivity = duduclaw_memory::read_sensitivity_metadata(Some(meta));
            assert!(
                sensitivity.allowed_in_session(is_dm),
                "a private 1:1 session must see every footprint fact"
            );
            if sensitivity == Sensitivity::Personal {
                checked_personal += 1;
                assert!(
                    !sensitivity.allowed_in_session(is_group),
                    "Personal footprint fact ({}) must be withheld from a group session",
                    entry.content
                );
            } else {
                checked_internal += 1;
                assert!(
                    sensitivity.allowed_in_session(is_group),
                    "Internal footprint fact ({}) must remain visible in a group session",
                    entry.content
                );
            }
        }
        // Sanity: both branches of the assertion were actually exercised.
        assert_eq!(checked_personal, 2, "daily_active_app + active_hours");
        assert_eq!(checked_internal, 1, "active_directory");
    }

    // ── distill_and_reset: day-boundary trigger + supersession chain ──────

    #[tokio::test]
    async fn distill_and_reset_writes_and_supersedes_across_days() {
        let home = tempfile::tempdir().unwrap();
        let agent = "agent-x";
        let tracker = FootprintTracker::new(home.path().to_path_buf(), enabled_set(agent));

        // Day 1: VSCode then Chrome, one file event.
        let day1_a = utc(2026, 7, 20, 9, 0);
        let day1_b = utc(2026, 7, 20, 9, 30);
        tracker.note_event(
            &AutopilotEvent::OsFrontmostEvent {
                agent_id: agent.into(),
                app: "VSCode".into(),
                window_title: "".into(),
                prev_app: "".into(),
            },
            day1_a,
        );
        tracker.note_event(
            &AutopilotEvent::OsFrontmostEvent {
                agent_id: agent.into(),
                app: "Chrome".into(),
                window_title: "".into(),
                prev_app: "VSCode".into(),
            },
            day1_b,
        );
        tracker.note_event(
            &AutopilotEvent::OsFileEvent {
                agent_id: agent.into(),
                path: "/Users/me/proj/src/main.rs".into(),
                change: "modified".into(),
            },
            day1_a,
        );

        // Distill triggers on day 2 (day1's bucket is now strictly in the past).
        let day2 = utc(2026, 7, 21, 9, 0);
        tracker.distill_and_reset(agent, day2).await;

        let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
        let history = engine
            .get_history(agent, FOOTPRINT_SUBJECT, PREDICATE_DAILY_ACTIVE_APP)
            .await
            .unwrap();
        assert_eq!(history.len(), 1, "first distillation writes one row");
        assert!(history[0].content.contains("VSCode"));
        assert!(history[0].valid_until.is_none(), "currently valid");

        // The still-open "Chrome" session was carried into day2's bucket at
        // the flush point — confirm by switching apps and distilling again.
        let day2_switch = utc(2026, 7, 21, 9, 5);
        tracker.note_event(
            &AutopilotEvent::OsFrontmostEvent {
                agent_id: agent.into(),
                app: "Terminal".into(),
                window_title: "".into(),
                prev_app: "Chrome".into(),
            },
            day2_switch,
        );
        let day3 = utc(2026, 7, 22, 9, 0);
        tracker.distill_and_reset(agent, day3).await;

        let history2 = engine
            .get_history(agent, FOOTPRINT_SUBJECT, PREDICATE_DAILY_ACTIVE_APP)
            .await
            .unwrap();
        assert_eq!(history2.len(), 2, "supersession chain has both days");
        assert!(
            history2[0].valid_until.is_some(),
            "day1's fact must be closed out"
        );
        assert_eq!(
            history2[0].superseded_by.as_deref(),
            Some(history2[1].id.as_str())
        );
        assert!(history2[1].valid_until.is_none(), "day2's fact is current");
        assert!(
            history2[1].content.contains("Chrome"),
            "the carried-forward Chrome session must appear in day2's stats: {}",
            history2[1].content
        );

        // Retrieval-side check: the currently-valid fact is reachable through
        // the ordinary FTS `search_layer` path (proves it's NOT a special
        // side-table only this module can read).
        let found = engine
            .search_layer(
                agent,
                "Chrome",
                &duduclaw_core::types::MemoryLayer::Semantic,
                10,
            )
            .await
            .unwrap();
        assert!(
            found.iter().any(|e| e.content.contains("Chrome")),
            "day2's daily_active_app fact must be findable via search_layer"
        );
    }

    #[tokio::test]
    async fn distill_and_reset_same_day_is_noop() {
        let home = tempfile::tempdir().unwrap();
        let agent = "agent-x";
        let tracker = FootprintTracker::new(home.path().to_path_buf(), enabled_set(agent));
        let t = utc(2026, 7, 20, 9, 0);
        tracker.note_event(
            &AutopilotEvent::OsFrontmostEvent {
                agent_id: agent.into(),
                app: "VSCode".into(),
                window_title: "".into(),
                prev_app: "".into(),
            },
            t,
        );
        // Same UTC day — must not distill (no db file should even be needed).
        tracker
            .distill_and_reset(agent, utc(2026, 7, 20, 20, 0))
            .await;
        assert!(!home.path().join("memory.db").exists());
    }

    #[tokio::test]
    async fn distill_and_reset_unknown_agent_is_noop() {
        let home = tempfile::tempdir().unwrap();
        let tracker = FootprintTracker::new(home.path().to_path_buf(), enabled_set("agent-x"));
        tracker
            .distill_and_reset("agent-x", utc(2026, 7, 21, 9, 0))
            .await;
        assert!(!home.path().join("memory.db").exists());
    }

    #[tokio::test]
    async fn distill_and_reset_empty_bucket_writes_nothing() {
        // A bucket can only be genuinely empty here via white-box setup (the
        // public `note_event` API always leaves at least an hour-count entry
        // behind) — inject one directly to prove `AgentDayStats::is_empty`
        // actually short-circuits the write, not just that it's unreachable
        // in practice.
        let home = tempfile::tempdir().unwrap();
        let agent = "agent-x";
        let tracker = FootprintTracker::new(home.path().to_path_buf(), enabled_set(agent));
        {
            let mut map = tracker.state.lock().unwrap();
            map.insert(
                agent.to_string(),
                AgentDayStats::new(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap()),
            );
        }
        tracker
            .distill_and_reset(agent, utc(2026, 7, 21, 9, 0))
            .await;
        assert!(
            !home.path().join("memory.db").exists(),
            "an empty day must never touch the memory db"
        );
        // The bucket is still rolled forward to "today" even though nothing
        // was written, so a later real event on the new day isn't lost by
        // being folded into a stale date.
        let map = tracker.state.lock().unwrap();
        assert_eq!(
            map.get(agent).unwrap().date,
            NaiveDate::from_ymd_opt(2026, 7, 21).unwrap()
        );
    }

    #[tokio::test]
    async fn distill_and_reset_single_switch_day_still_distills() {
        // A day with exactly one observed frontmost switch (no closing switch
        // yet) is NOT empty at flush time — the open app's elapsed-so-far
        // seconds get folded in, so it must still distill.
        let home = tempfile::tempdir().unwrap();
        let agent = "agent-x";
        let tracker = FootprintTracker::new(home.path().to_path_buf(), enabled_set(agent));
        tracker.note_event(
            &AutopilotEvent::OsFrontmostEvent {
                agent_id: agent.into(),
                app: "VSCode".into(),
                window_title: "".into(),
                prev_app: "".into(),
            },
            utc(2026, 7, 20, 9, 0),
        );
        tracker
            .distill_and_reset(agent, utc(2026, 7, 21, 9, 0))
            .await;
        assert!(
            home.path().join("memory.db").exists(),
            "a day with at least one observed switch must distill (elapsed time counts)"
        );
    }
}
