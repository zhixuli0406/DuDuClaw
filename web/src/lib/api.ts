import { client } from './ws-client';
import { migrateScanArgs, migrateApplyArgs } from './migrate';
import type { RunDetail, RunSummary } from './run-transcript';

// Type definitions matching Rust types
export interface AgentInfo {
  name: string;
  display_name: string;
  role: 'main' | 'specialist' | 'worker';
  // Note: the backend also reports "archived" here, but archive state is read
  // from the dedicated `archived` boolean below (the narrow union keeps the many
  // status-driven components — poses, world stage, assignee — unchanged).
  status: 'active' | 'paused' | 'terminated';
  trigger: string;
  icon: string;
  reports_to: string;
  /** WP4 — archived (recoverable off-board). Hidden from the roster unless
   *  `agents.list` is called with `include_archived: true`. */
  archived?: boolean;
  /** WP4 — an uploaded avatar image exists on disk. The bytes are NOT in the
   *  list payload (kept light); resolve them via the lightweight `agents.avatar`
   *  RPC (see the `agent-avatar-store`). */
  has_avatar?: boolean;
  /** WP7 — the department this AI staff member belongs to (company → department
   *  → personal layering). Empty/absent = no department. */
  department?: string;
  /** Wardrobe (衣帽間) composition. `null`/absent = never dressed — surfaces
   *  render the seeded default look. Shape mirrors `lib/outfit.ts`. */
  outfit?: import('./outfit').AgentOutfit | null;
}

export interface AgentBudget {
  monthly_limit_cents: number;
  spent_cents: number;
  warn_threshold_percent: number;
  hard_stop: boolean;
}

export interface AgentLocalModel {
  model: string;
  backend: string;
  context_length: number;
  gpu_layers: number;
  prefer_local: boolean;
  use_router: boolean;
}

export interface AgentModel {
  preferred: string;
  fallback: string;
  account_pool: string[];
  api_mode?: string;
  local?: AgentLocalModel | null;
}

export interface AgentSticker {
  enabled: boolean;
  probability: number;
  intensity_threshold: number;
  cooldown_messages: number;
  expressiveness: 'minimal' | 'moderate' | 'expressive';
}

export interface AgentEvolution {
  gvu_enabled: boolean;
  cognitive_memory: boolean;
  skill_auto_activate: boolean;
  skill_security_scan: boolean;
  max_silence_hours: number;
}

export interface AgentDetail extends AgentInfo {
  budget: AgentBudget;
  model: AgentModel;
  sandbox_enabled?: boolean;
  network_access?: boolean;
  heartbeat: {
    enabled: boolean;
    interval_seconds: number;
    last_run?: string;
    next_run?: string;
  };
  skills: string[];
  permissions: Record<string, boolean>;
  sticker?: AgentSticker;
  evolution?: AgentEvolution;
  proactive?: ProactiveSettings;
  /** WP4 — the uploaded avatar as an inline data URI, or null when none.
   *  `agents.inspect` returns this alongside the full detail; the avatar store
   *  uses the lighter `agents.avatar` RPC when it only needs the image. */
  avatar?: string | null;
  /** [capabilities] block — returned by agents.inspect so the capability editor
   *  (incl. the Progent policy rules) can prefill existing values. */
  capabilities?: AgentCapabilities;
  /** [os_watch] table — returned raw by agents.inspect so the OS-native watch
   *  editor can prefill the operator's own paths/ignore/debounce. Null/absent
   *  when the agent declares no `[os_watch]`. */
  os_watch?: OsWatchConfig | null;
  /** [runtime] block — returned by agents.inspect so the runtime editor can
   *  prefill existing values. Emits ONLY keys present in agent.toml, so an
   *  absent `pty_pool_enabled` (vs. an explicit `false`) is meaningful: it
   *  gates the one-time PTY-pool OAuth default-enable materialization. */
  runtime?: {
    provider?: string;
    fallback?: string;
    pty_pool_enabled?: boolean;
    worker_managed?: boolean;
  };
  /** [research] table — returned by agents.inspect so the automation tab's
   *  self-study toggle can prefill the agent's own value (belief loop ×
   *  goal contract gap 2, design-market-belief-loop-2026-08.md §3). Always
   *  present with concrete values (defaults `self_study: false`,
   *  `self_study_hour: 20`) — unlike `os_watch`, there is no "unset" state
   *  to distinguish from a default. */
  research?: {
    self_study: boolean;
    self_study_hour: number;
  };
}

export interface VoiceSettings {
  asr_provider: string;
  tts_provider: string;
  asr_language: string;
  tts_voice: string;
  voice_reply_enabled: boolean;
}

export interface ProactiveSettings {
  enabled: boolean;
  check_interval: string;
  /** @deprecated Legacy numeric pair — deliberately NOT read by the W2-4
   *  notification gate (`notify_governance.rs`); kept only because the
   *  typed `ProactiveConfig` struct still round-trips it. Use `quiet_hours`
   *  (the `HH:MM-HH:MM` string below) for anything suppression-related. */
  quiet_hours_start: number;
  /** @deprecated see `quiet_hours_start`. */
  quiet_hours_end: number;
  max_messages_per_hour: number;
  notify_channel: string;
  notify_chat_id: string;
  /** Optional thread/topic ID within the chat (Discord thread / TG topic). */
  notify_thread_id?: string;
  /** W2-8 — the agent's OWN raw `[proactive] quiet_hours` value
   *  (`HH:MM-HH:MM`), empty when unset. This — NOT `quiet_hours` below — is
   *  what an edit form must read/write: prefilling from the effective
   *  (fallen-back) value and saving it unchanged would silently pin the
   *  deployment-wide default into this one agent's config. Write via
   *  `agents.update` `proactive.quiet_hours`; empty string clears it. */
  quiet_hours_own?: string;
  /** Effective quiet-hours window (`HH:MM-HH:MM`) after the agent → global
   *  `config.toml [notify] quiet_hours` fallback (W2-4), or `null` when
   *  nothing is ever held back. Display-only — do not write this field
   *  back, see `quiet_hours_own`. */
  quiet_hours?: string | null;
  /** zh-TW sentence stating exactly what is deferred vs. delivered
   *  immediately during `quiet_hours` above (F10: a suppression rule the
   *  UI cannot state is a silent failure). Display-only, `null` when no
   *  window is in force. */
  quiet_hours_note?: string | null;
}

export interface ChannelStatus {
  name: string;
  connected: boolean;
  last_connected?: string;
  error?: string;
}

/** W2-2 (E1) — a channel's behavior settings (`channels.config_get/set`). */
export interface ChannelConfigSettings {
  mention_only: boolean;
  auto_thread: boolean;
  allowed_channels: string[];
  allowed_guilds: string[];
  agent_override: string;
  response_mode: 'embed' | 'plain' | 'auto';
  /** Stored as a string ("60"|"1440"|"4320"|"10080"); `null` = never set. */
  thread_archive_minutes: string | null;
}

/** W2-2 (E2) — a channel's access-control settings (`channels.access_get/set`). */
export interface ChannelAccessSettings {
  require_pairing: boolean;
  allowed_users: string[];
  blocked_users: string[];
  /** Who may run admin-gated chat commands (`!STOP` / `!STOP ALL` / `!RESUME`). */
  admin_users: string[];
}

export interface CliCredentialInfo {
  runtime: 'codex' | 'gemini' | 'grok';
  /** Display path of the credential store, e.g. "~/.grok/auth.json". */
  store: string;
  installed: boolean;
  present: boolean;
  /** Epoch seconds of the credential file's last write, when present. */
  modified_at: number | null;
}

export interface AccountInfo {
  id: string;
  auth_method: 'apikey' | 'oauth';
  account_type?: string; // legacy alias
  priority: number;
  is_healthy: boolean;
  is_available: boolean;
  spent_this_month: number;
  monthly_budget_cents: number;
  total_requests: number;
  label: string;
  email: string;
  subscription: string;
  expires_at: string | null;
  days_until_expiry: number | null;
}

export interface BudgetSummary {
  total_budget_cents: number;
  total_spent_cents: number;
  accounts: AccountInfo[];
}

export interface DoctorCheck {
  name: string;
  status: 'pass' | 'warn' | 'fail';
  message: string;
  can_repair: boolean;
  repair_hint?: string;
}

/** Product form-factor, orthogonal to the license `edition` string. */
export type EditionProfile = 'personal' | 'enterprise';

export interface SystemStatus {
  version: string;
  uptime_seconds: number;
  agents_count: number;
  channels_connected: number;
  gateway_address: string;
  /**
   * Product form-factor (personal|enterprise). Controls whether the dashboard
   * shows enterprise management surfaces. Absent on older gateways → treat as
   * enterprise (show everything) for backward compatibility.
   */
  edition_profile?: EditionProfile;
  /**
   * R2 (2026-08): whether this gateway is the DuDuClaw appliance image — a
   * direct forward of `duduclaw_core::is_appliance()`, the same authority
   * `device.status` gates on. Unlike `device.status` (admin + appliance
   * only), `system.status` carries no admin gate, so this is the one
   * appliance signal every authenticated role can read; `useIsAppliance`
   * reads this field. Absent on older gateways → treat as `false` (fail
   * closed, matches the hook's own fail-closed default).
   */
  is_appliance?: boolean;
}

/** Response of `system.autostart.status` / `system.autostart.set` — the
 *  login/boot registration state of the gateway (launchd LaunchAgent on macOS,
 *  systemd user unit on Linux, HKCU Run key on Windows). */
export interface AutostartStatus {
  supported: boolean;
  enabled: boolean;
  method: 'launchd' | 'systemd-user' | 'windows-run-key' | 'unsupported';
  /** Human-readable registration location (plist/unit path, registry key). */
  detail: string;
}

export interface LogEntry {
  level: 'trace' | 'debug' | 'info' | 'warn' | 'error';
  target: string;
  message: string;
  timestamp: string;
  agent_id?: string;
}

export interface MemoryEntry {
  id: string;
  agent_id: string;
  content: string;
  timestamp: string;
  tags: string[];
  /** Cognitive layer: `episodic` | `semantic` | `procedural`. */
  layer?: string;
  /** What produced this memory (e.g. `footprint_distill`, `micro_reflection`). */
  source_event?: string;
  /** Importance score 0–10 (decay resistance). */
  importance?: number;
  /** How many times this entry has been retrieved. */
  access_count?: number;
  /** RFC-3339 timestamp of the last recall, or null if never recalled. */
  last_accessed?: string | null;
  /**
   * 0–1 — how likely this memory is to still be retrievable right now. The
   * gateway computes it from the very same curve the archival job scores
   * against, so the freshness the page shows and the archival decision can
   * never disagree. Shown to users as a plain-language state, never as a number.
   */
  retrievability?: number;
  /**
   * How many days of silence it takes for this memory to fade to about a third
   * of its strength. Grows every time the memory is recalled.
   */
  stability_days?: number;
}

/** One band of the freshness histogram in `memory.decay_overview`. */
export interface MemoryFreshnessBucket {
  /** Stable wire key: `fresh` | `stable` | `fading` | `archiving`. */
  key: string;
  count: number;
  /** Inclusive lower bound of the band, for the legend. */
  min_retrievability: number;
}

/** One day of the memory-accumulation trend. */
export interface MemoryTrendPoint {
  /** `YYYY-MM-DD`. */
  date: string;
  /** Memories recorded that day. */
  added: number;
  /** Running pile size at the end of that day. */
  total: number;
}

export interface MemoryDecayOverview {
  total: number;
  scanned: number;
  /** True when the scan cap bound — the numbers describe a recent slice only. */
  truncated: boolean;
  buckets: MemoryFreshnessBucket[];
  /** Faintest first — the memories closest to being filed away. */
  fading_soon: MemoryEntry[];
  /** Most-recalled first; entries never recalled are excluded. */
  most_recalled: MemoryEntry[];
  trend: MemoryTrendPoint[];
  window_days: number;
  /** Retrievability at which the archival job files a memory away. */
  archive_threshold: number;
}

export interface KeyFactEntry {
  id: string;
  agent_id: string;
  fact: string;
  channel: string;
  chat_id: string;
  source_session: string;
  timestamp: string;
  access_count: number;
}

export interface WikiPageMeta {
  path: string;
  title: string;
  updated: string;
  tags: string[];
}

export interface WikiSearchHit {
  path: string;
  title: string;
  score: number;
  context_lines: string[];
}

/**
 * WP5c — one auto-filed knowledge page, as listed by the curation station's
 * 自動建檔 audit tab. `sources` carries the conversation chain that produced
 * the page; `revision_count` mirrors the page's in-body revision log.
 */
export interface AutoWikiPage {
  path: string;
  title: string;
  updated: string;
  /** charter | sop | spec | policy | reference */
  doc_type: string;
  /** Localised label already resolved by the backend (章程 / 流程 / …). */
  doc_type_label: string;
  sources: string[];
  revision_count: number;
  trust: number;
}

export interface WikiLintReport {
  total_pages: number;
  index_entries: number;
  orphan_pages: string[];
  broken_links: [string, string][];
  stale_pages: string[];
  healthy: boolean;
}

export interface WikiStats {
  exists: boolean;
  total_pages: number;
  by_directory: Record<string, number>;
  most_recent?: {
    title: string;
    path: string;
    updated: string;
  };
}

export interface WikiTrustRow {
  page_path: string;
  agent_id: string;
  trust: number;
  citation_count: number;
  error_signal_count: number;
  success_signal_count: number;
  last_signal_at: string | null;
  last_verified: string | null;
  do_not_inject: boolean;
  locked: boolean;
  updated_at: string;
}

export interface WikiTrustHistoryRow {
  ts: string;
  old_trust: number;
  new_trust: number;
  applied_delta: number;
  trigger: string;
  conversation_id: string | null;
  composite_error: number | null;
  signal_kind: string;
}

export interface SharedWikiStats {
  exists: boolean;
  total_pages: number;
  by_author: Record<string, number>;
  by_directory: Record<string, number>;
  most_recent?: {
    title: string;
    path: string;
    updated: string;
    author: string | null;
  };
}

export interface SkillInfo {
  name: string;
  agent_id?: string;
  content: string;
  /** Which layer the skill came from: company-wide / department / this staffer. */
  scope?: 'global' | 'department' | 'agent';
  security_status?: 'pass' | 'warn' | 'fail';
}

/**
 * One directory the gateway actually walked while answering `skills.list`.
 *
 * Without this an empty list is unfalsifiable from the browser — "no skills
 * installed" and "the folder I expected isn't the folder that gets read" look
 * identical. The empty state prints these so a customer can self-diagnose.
 */
export interface SkillScanPath {
  layer: 'global' | 'department' | 'agent';
  path: string;
  exists: boolean;
  count: number;
}

/** Aggregate (`skills.list` with no `agent_id`) — every layer, every staffer. */
export interface SkillsListAll {
  global_skills: SkillInfo[];
  agents: Array<{ agent_id: string; display_name?: string; skills: SkillInfo[] }>;
  scanned?: SkillScanPath[];
}

export interface HeartbeatInfo {
  agent_id: string;
  enabled: boolean;
  interval_seconds: number;
  cron: string;
  last_run?: string;
  next_run?: string;
  total_runs: number;
  active_runs: number;
  max_concurrent: number;
}

export interface AuditEvent {
  timestamp: string;
  event_type: string;
  agent_id: string;
  severity: 'info' | 'warning' | 'critical';
  details: Record<string, unknown>;
}

// ── Credential hygiene (WP-K) ──
// `security.credential_hygiene` / `security.credential_cleanup` never return
// the secret value itself — only the TOML path, twin status and severity.
export interface CredentialFinding {
  /** Dotted/bracket TOML path, e.g. "accounts[0].oauth_token". Never a value. */
  path: string;
  /** True when an encrypted `<field>_enc` twin already exists — safe for
   *  `security.credential_cleanup` to auto-remove. False = manual only. */
  has_enc_twin: boolean;
  severity: 'info' | 'low' | 'medium' | 'high';
}
export interface CredentialHygieneReport {
  clean: boolean;
  count: number;
  findings: CredentialFinding[];
}
export interface CredentialCleanupResult {
  cleaned: boolean;
  removed_paths: string[];
  backup_path?: string;
  message?: string;
}

// ── Credential inventory (WP-H1 P1) ──
// `security.credential_inventory` is the structured answer to "which settings
// use a secret:// reference, which are still plaintext or _enc". It is built
// from `describe()`, which never resolves a credential and never holds one, so
// there is no value in this payload to mask.
/** Where a credential's value comes from. `ambiguous` is a lone field that is
 *  either ciphertext or plaintext and cannot be told apart without decrypting
 *  (the legacy `[integrations.*] secret` shape). */
export type CredentialSourceKind =
  | 'unset'
  | 'inline'
  | 'legacy'
  | 'env'
  | 'keychain'
  | 'file'
  | 'vault'
  | 'onepassword'
  | 'infisical'
  | 'local'
  | 'ambiguous';
export interface CredentialEntry {
  /** TOML path of the logical field, e.g. "channels.telegram_bot_token" or
   *  "agents.<id>.channels.discord.bot_token". Never a value. */
  path: string;
  configured: boolean;
  source: CredentialSourceKind;
  /** Non-secret description: "encrypted(keyfile)", "env:TG_TOKEN",
   *  "keychain:duduclaw/telegram", "plaintext(legacy)". */
  source_label: string;
  /** False for external references — those rotate in their own backend. */
  writable: boolean;
  /** A plaintext twin sits next to an encrypted twin (design §1.5). */
  residue: boolean;
}
export interface CredentialInventoryReport {
  entries: CredentialEntry[];
  total: number;
  configured: number;
  /** Fields sourced from an external `secret://` reference. */
  referenced: number;
  residue: number;
  plaintext: number;
}

// ── Delegation permissions (WP21 §2.8) ──
/** How the gateway decides whether one AI staffer may hand work to another. */
export type DelegationPolicy = 'department' | 'hierarchy' | 'open';

export interface DelegationSettings {
  policy: DelegationPolicy;
  /** Unordered agent-id pairs — a pair permits delegation in both directions. */
  allow: Array<[string, string]>;
  /** Operator-facing notes about values the gateway had to fall back on. */
  warnings: string[];
}

/** v1.54 global `[task_forward_model]` switches (calibrated prediction + held-out learning gate). */
export interface TaskForwardModelSettings {
  /** Master switch — task-level prediction itself. */
  enabled: boolean;
  /** Score every prediction against the real outcome (Brier/RPS + Murphy). */
  calibration_enabled: boolean;
  /** Inductive lessons must earn adoption out-of-sample before injection. */
  held_out_gate_enabled: boolean;
}

// ── Unified audit log (merges security, tool_call, channel_failure, feedback) ──
export type UnifiedAuditSource = 'security' | 'tool_call' | 'channel_failure' | 'feedback';

export interface UnifiedAuditEvent {
  timestamp: string;
  source: UnifiedAuditSource;
  event_type: string;
  agent_id: string;
  severity: 'info' | 'warning' | 'critical';
  summary: string;
  details: Record<string, unknown>;
}

export interface UnifiedAuditResponse {
  events: UnifiedAuditEvent[];
  source_counts: Record<UnifiedAuditSource, number>;
  total: number;
}

// ── W3-1 human takeover (read-only) — `takeover.list`. Surfaced in the W3-4
// developer panel's 系統 tab so a manager watching the fleet can see who is
// currently holding a conversation without leaving the page they're on. No
// write twin by design (see the gateway dispatch comment on `takeover.list`) —
// starting/extending/ending a takeover is a channel-side act.
export interface TakeoverRecord {
  /** Channel session key — the same shape `chat.sessions.list` uses. */
  conversation: string;
  channel: string;
  /** Human-readable channel name (already localized server-side). */
  channel_label: string;
  chat_id: string;
  agent_id: string;
  /** Display name only — `holder_user_id` is deliberately never sent here. */
  holder_display: string;
  started_at: string;
  until: string;
  /** Whole minutes left, floored at 0 (never negative). */
  minutes_left: number;
  claimed_task_ids: string[];
}

export interface TakeoverListResponse {
  count: number;
  items: TakeoverRecord[];
}

export interface SkillIndexEntry {
  name: string;
  description: string;
  tags: string[];
  author: string;
  url: string;
  compatible: string[];
}

// ── Agent Reliability Dashboard (W20-P0) ─────────────────────────────────────

export interface ReliabilitySummary {
  /** Agent identifier. */
  agent_id: string;
  /** Measurement window in days (default 7). */
  window_days: number;
  /** Mean per-event-type success rate (0.0–1.0). */
  consistency_score: number;
  /** Overall success rate (0.0–1.0). */
  task_success_rate: number;
  /** Fraction of skill_activate events (0.0–1.0). */
  skill_adoption_rate: number;
  /** Fraction of llm_fallback_triggered events (0.0–1.0). */
  fallback_trigger_rate: number;
  /** Total events counted in the window. */
  total_events: number;
  /** RFC3339 timestamp when the summary was generated. */
  generated_at: string;
}

// ── Task Board types ────────────────────────────────────────

// Iterative Kanban (v1.45): `review` (goal-mode acceptance pending) and
// `revising` (judge-rejected, awaiting the next round) are now first-class
// board columns. `pending` / `cancelled` are transient/terminal states the
// backend may also return; the board simply doesn't give them a column.
export type TaskStatus =
  | 'todo'
  | 'in_progress'
  | 'review'
  | 'revising'
  | 'done'
  | 'blocked'
  | 'needs_human'
  | 'failed'
  | 'pending'
  | 'cancelled';
export type TaskPriority = 'low' | 'medium' | 'high' | 'urgent';

/** H11 — why a goal task is parked `needs_human`. Mirrors the Rust
 *  `pause_reason::PauseReason` wire tokens exactly (that enum's
 *  `wire_tokens_are_pinned` test guards the other side of this contract).
 *  `'unknown'` is what every unclassified / legacy row resolves to. */
export type PauseReasonToken =
  | 'no_progress'
  | 'budget_exhausted'
  | 'blocked_needs_decision'
  | 'infra'
  | 'restart'
  | 'unknown';

export interface TaskInfo {
  id: string;
  title: string;
  description: string;
  status: TaskStatus;
  priority: TaskPriority;
  assigned_to: string;
  created_by: string;
  created_at: string;
  updated_at: string;
  completed_at?: string;
  blocked_reason?: string;
  /** Latest judge feedback / escalation reason (populated for needs_human). */
  judge_feedback?: string;
  /** H11 — closed classification of WHY the task parked `needs_human`, as a
   *  stable token the UI maps to `goals.pauseReason.<token>`. The server
   *  always resolves it, so an unrecognised/legacy row arrives as
   *  `'unknown'` rather than absent. Only meaningful while
   *  `status === 'needs_human'`; cleared once a human resolves the pause. */
  pause_reason?: PauseReasonToken;
  /** I-1c "想一想": a generated execution plan awaiting human approval.
   *  Only meaningful while `status === 'needs_human'` — the server clears it
   *  after the first dispatch that follows approval, and scopes it to
   *  `needs_human` the same defensive way as `pause_reason`. */
  plan_pending?: string | null;
  parent_task_id?: string;
  tags: string[];
  message_id?: string;
  // ── Iterative Kanban (v1.45) ──────────────────────────────
  /** Judge-rejection round counter (0 = first attempt). */
  revision_round?: number;
  /** Set once `revision_round` reaches the soft cap — diminishing returns. */
  diminishing?: boolean;
  /** Cumulative agent processing seconds (the "agent clock"). */
  agent_seconds?: number;
  /** Lease deadline (RFC3339); a past value with an in_progress task = stale. */
  lease_expires_at?: string;
  // ── Goal-loop surface (2026-08-14 /goals page) ────────────
  /** True for autonomous goal tasks driven by the goal loop. */
  goal_mode?: boolean;
  /** Acceptance contract the judge verifies against. */
  acceptance_criteria?: string | null;
  /** The current round's submitted result (judge input). */
  result_summary?: string | null;
  retry_count?: number;
  max_retries?: number;
  /** Worker lease holder, or the human decider after a takeover. */
  claimed_by?: string | null;
  /** Parsed `goal_state_json`: confirmed_facts / pending_hypotheses. */
  goal_state?: { confirmed_facts?: string[]; pending_hypotheses?: string[] } | null;
  // ── Goal assignment form v2 (design-market-belief-loop-2026-08.md §6,
  // G1, 2026-08-14) ─────────────────────────────────────────
  /** Per-goal wall-clock deadline (RFC3339), when the assign form set a
   *  `duration_hours`. `null` ⇒ only the global wall-clock budget applies. */
  deadline_at?: string | null;
  /** Per-goal risk boundary text the user explicitly typed into the assign
   *  form. `null` ⇒ the deployment baseline boundary applies instead. */
  risk_boundary?: string | null;
  // ── W2-3 reverse handoff (E8) ─────────────────────────────
  /** Originating channel of a `/goal` command (e.g. `telegram`), when known.
   *  `null`/absent for tasks not launched from a channel conversation. */
  channel?: string | null;
  /** Server-resolved "open in `channel`" URL, or `null` when nothing could
   *  be constructed for this platform (never a raw chat/message id — see
   *  `channel_link.rs`). Render the button ONLY when this is a non-empty
   *  string. */
  channel_link?: string | null;
  // ── I-3b task list operations (2026-08-15) ────────────────
  /** Recoverable off-board — hidden from every general listing
   *  (`tasks.list` / `tasks.list_page` with `archived` unset or `false`) by
   *  default. */
  archived?: boolean;
  /** Sorted first within `tasks.list_page` results (`ORDER BY pinned DESC`). */
  pinned?: boolean;
}

/** One judge-review round of a goal-mode task (Iterative Kanban timeline). */
export interface TaskIteration {
  round: number;
  dispatched_at: string;
  submitted_at?: string | null;
  judged_at?: string | null;
  verdict?: 'accepted' | 'rejected' | 'escalated' | null;
  judge_feedback?: string | null;
  feedback_class?: string | null;
  /** Per-aspect MAV panel results — null for deterministic rejections and
   *  rows judged before 2026-08-14. */
  aspects?: Array<{ name: string; pass: boolean; reason: string }> | null;
  /** How many times this round was dispatched (stall re-dispatches). */
  dispatch_count?: number;
  /** Same-(state, action) repeat streak at dispatch time (≥2 = no progress). */
  repeat_streak?: number | null;
}

/** Per-agent flow metrics (Iterative Kanban analytics). */
export interface AgentFlowMetrics {
  agent_id: string;
  goal_tasks: number;
  finished: number;
  first_pass_yield: number;
  avg_rounds: number;
  avg_agent_seconds: number;
  avg_cycle_seconds: number;
  review_queue_depth: number;
}

/** Board-level + per-agent flow metrics returned by tasks.flow_metrics. */
export interface FlowMetrics {
  agents: AgentFlowMetrics[];
  review_queue_depth: number;
  review_wip_limit: number;
  accepts_last_7d: number;
  avg_daily_accepts_7d: number;
}

// RFC-24 Decision Continuity
export interface DecisionOption {
  key: string;
  content: string;
}

export interface DecisionInfo {
  id: string;
  question: string;
  options: DecisionOption[];
  created_at?: string | null;
}

export interface TaskCreateParams {
  title: string;
  description?: string;
  priority?: TaskPriority;
  /** Optional — omit for an unassigned task (never auto-dispatched, Bug#4). */
  assigned_to?: string;
  tags?: string[];
  parent_task_id?: string;
}

export interface TaskUpdateParams {
  title?: string;
  description?: string;
  status?: TaskStatus;
  priority?: TaskPriority;
  assigned_to?: string;
  blocked_reason?: string;
  tags?: string[];
}

// ── Co-edited plan types (U4) ───────────────────────────────

export type PlanStatus = 'active' | 'done' | 'archived';
export type PlanStepStatus = 'todo' | 'doing' | 'done' | 'skipped';
export type PlanAssigneeKind = 'user' | 'agent';

/** A shared plan co-edited by the user and one AI employee. */
export interface PlanInfo {
  id: string;
  title: string;
  description: string;
  /** Owning AI employee. */
  agent_id: string;
  goal_id?: string | null;
  status: PlanStatus;
  created_by: string;
  created_at: string;
  updated_at: string;
  /** Progress counters — present on `plans.list` responses. */
  steps_total?: number;
  steps_done?: number;
}

export interface PlanStep {
  id: string;
  plan_id: string;
  text: string;
  assignee_kind: PlanAssigneeKind;
  /** User id (kind=user) or agent id (kind=agent). Empty = unassigned. */
  assignee: string;
  status: PlanStepStatus;
  step_order: number;
  created_at: string;
  updated_at: string;
}

export interface PlanCreateParams {
  title: string;
  agent_id: string;
  description?: string;
  goal_id?: string;
  steps?: Array<{ text: string; assignee_kind?: PlanAssigneeKind; assignee?: string }>;
}

export interface PlanStepUpdateParams {
  text?: string;
  status?: PlanStepStatus;
  assignee_kind?: PlanAssigneeKind;
  assignee?: string;
  /** Target display index — reorder. */
  position?: number;
}

// ── Activity Feed types ─────────────────────────────────────

export type ActivityType =
  | 'task_created'
  | 'task_completed'
  | 'task_blocked'
  | 'task_assigned'
  | 'agent_reply'
  | 'agent_created'
  | 'memory_distilled'
  | 'wiki_written'
  | 'skill_learned'
  | 'evolution_triggered'
  | 'autopilot_triggered'
  | 'autopilot_lag'
  | 'error'
  // W2-8 — `channel_alerts.rs` (W2-4) writes these two Activity Feed rows
  // (a channel outage crossing the alert threshold, and its later
  // resolution). Both existed on the wire before this type declared them —
  // they rendered with the generic fallback icon until now.
  | 'channel_send_failure_alert'
  | 'channel_recovered';

export interface ActivityEvent {
  id: string;
  type: ActivityType;
  agent_id: string;
  task_id?: string;
  summary: string;
  timestamp: string;
  metadata?: Record<string, unknown>;
}

// ── Work Timeline types (G11) ───────────────────────────────

/** Lane kinds the company timeline can carry. */
export type TimelineKind =
  | 'task'
  | 'delegation'
  | 'heartbeat'
  | 'skill'
  | 'autopilot'
  | 'governance'
  | 'activity';

/** One Gantt row of the company work timeline — derived server-side from real
 *  timestamps only. An instant is `ended_at === started_at` (rendered as a dot). */
export interface TimelineRow {
  agent_id: string;
  kind: TimelineKind;
  label: string;
  /** RFC3339. */
  started_at: string;
  /** null = still running (bar extends to now); `=== started_at` = instant. */
  ended_at: string | null;
  status: string;
  ref_id: string;
}

export interface TimelineListResult {
  rows: TimelineRow[];
  /** Server row cap; when `truncated` is true the window holds more than `cap` rows. */
  cap: number;
  truncated: boolean;
  from: string;
  to: string;
}

// ── Live Canvas types (G15) ─────────────────────────────────

/** One stored canvas version (current or historical). */
export interface CanvasInfo {
  /** Monotonic version number (server-assigned, per push). */
  seq: number;
  agent_id: string;
  title: string;
  /**
   * Server-sanitized HTML. Empty string ⇒ the agent cleared the canvas.
   * Render ONLY via a sandboxed iframe (see lib/canvas-doc.ts) — never
   * dangerouslySetInnerHTML.
   */
  html: string;
  updated_at: string;
}

/** History metadata — no HTML body (versions can be up to 256 KB each). */
export interface CanvasVersionMeta {
  seq: number;
  title: string;
  updated_at: string;
  /** Sanitized HTML size in bytes (0 ⇒ cleared tombstone). */
  bytes: number;
}

export interface CanvasGetResult {
  agent_id: string;
  /** null ⇒ the agent has never pushed (or the requested seq is gone). */
  canvas: CanvasInfo | null;
  /** Retained versions, newest first (≤ 5). */
  history: CanvasVersionMeta[];
}

// ── Task comment types (L2) ─────────────────────────────────

/** A human-authored comment on a task (distinct from system activity events). */
export interface TaskComment {
  id: string;
  task_id: string;
  /** Authoring user id (from the authenticated session). */
  author_user: string;
  body: string;
  created_at: string;
}

// ── Autopilot types ─────────────────────────────────────────

export type AutopilotTriggerEvent =
  | 'task_created'
  | 'task_status_changed'
  | 'channel_message'
  | 'agent_idle'
  | 'schedule'
  // OS-native perception events (P2-4) — used by the OS-page rule templates.
  | 'os_file'
  | 'os_frontmost';

export type AutopilotActionType = 'delegate' | 'notify' | 'run_skill';

export interface AutopilotCondition {
  from_status?: TaskStatus;
  to_status?: TaskStatus;
  agent_id?: string;
  channel_type?: string;
  idle_minutes?: number;
  cron?: string;
}

export interface AutopilotAction {
  type: AutopilotActionType;
  agent_id: string;
  prompt_template?: string;
  skill_name?: string;
}

export interface AutopilotRule {
  id: string;
  name: string;
  enabled: boolean;
  trigger_event: AutopilotTriggerEvent;
  conditions: AutopilotCondition;
  action: AutopilotAction;
  created_at: string;
  last_triggered_at?: string;
  trigger_count: number;
}

export interface AutopilotCreateParams {
  name: string;
  trigger_event: AutopilotTriggerEvent;
  conditions: AutopilotCondition;
  action: AutopilotAction;
}

export interface AutopilotHistoryEntry {
  id: string;
  rule_id: string;
  rule_name: string;
  triggered_at: string;
  result: 'success' | 'failure';
  details?: string;
}

// ── Resident sensing observability (WP4) ────────────────────
// Backend: `ticks.sources` / `ticks.recent` RPCs (handlers.rs). Field names
// intentionally mirror the wire shape 1:1 — no dashboard-side renaming.

export interface TickDroppedCounts {
  rate_cap: number;
  unchanged: number;
  oversize: number;
  fetch_error: number;
  /** websocket sources only — binary frames the text-only pipeline refuses. */
  non_text: number;
  /**
   * Payloads that resolved none of the source's configured `json_fields` —
   * a feed's control/heartbeat frames. Always present; 0 for a source that
   * declares no `json_fields`.
   */
  no_fields: number;
}

export interface TickSourceStatus {
  id: string;
  kind: 'http_poll' | 'command' | 'file_tail' | 'websocket';
  enabled: boolean;
  interval_secs: number;
  max_events_per_minute: number;
  last_tick_ts: string | null;
  events_per_minute_approx: number;
  events_emitted_total: number;
  dropped: TickDroppedCounts;
}

export interface TickScreenCounts {
  pass: number;
  drop: number;
  unavailable: number;
}

export interface TicksSourcesResult {
  enabled: boolean;
  allow_command_sources: boolean;
  sources: TickSourceStatus[];
  screen: TickScreenCounts;
}

export interface TickRecordEntry {
  ts: string;
  fields: Record<string, unknown>;
  raw: string | null;
}

export interface TicksRecentResult {
  source: string;
  records: TickRecordEntry[];
}

// ── Skill Sharing types ─────────────────────────────────────

export interface SharedSkillInfo {
  name: string;
  description: string;
  shared_by: string;
  shared_at: string;
  adopted_by: string[];
  usage_count: number;
  tags: string[];
}

// RFC-26 Live Run Forking
export interface ForkSummary {
  fork_id: string;
  agent_id: string;
  merge_mode: string;
  resolved: boolean;
  winner: string | null;
  promoted: boolean;
  aggregate_spent_usd: number;
  created_at: string;
}

export interface ForkBranch {
  branch_id: string;
  steering: string | null;
  state: string;
  budget_usd: number;
  spent_usd: number;
  test_exit_code: number | null;
  output: string;
}

export interface ForkDetail {
  fork_id: string;
  agent_id: string;
  prompt: string;
  merge_mode: string;
  resolved: boolean;
  winner: string | null;
  promoted: boolean;
  branches: ForkBranch[];
}

export interface EvolutionMetrics {
  positive_feedback_ratio: number;
  prediction_error: number;
  user_correction_rate: number;
  contract_violations: number;
}

/** Tri-state hardware fit for one quant (`localmodels.*`). */
export type MarketFit = 'comfortable' | 'tight' | 'too_big';

export interface MarketQuant {
  filename: string;
  quant: string;
  size_bytes: number;
  shards?: string[];
  imatrix: boolean;
  fit: MarketFit;
  /** MoE only: fit when experts are offloaded to system RAM (cpu_moe). */
  fit_offload?: MarketFit;
}

export interface MarketModel {
  repo: string;
  name: string;
  publisher: string;
  downloads: number;
  likes: number;
  gated: boolean;
  params_b?: number;
  architecture?: string;
  moe: boolean;
  active_params_b?: number;
  context_length?: number;
  has_chat_template: boolean;
  languages: string[];
  recommended?: MarketQuant;
  quants: MarketQuant[];
}

export interface MarketHardware {
  gpu_name: string;
  gpu_type: string;
  vram_available_mb: number;
  ram_total_mb: number;
  ram_available_mb: number;
}

export interface MarketInstallJob {
  id: number;
  repo: string;
  filename: string;
  state: 'queued' | 'downloading' | 'completed' | 'failed' | 'cancelled';
  downloaded_bytes: number;
  total_bytes: number;
  error?: string;
  dest?: string;
}

/** Per-agent forward-model aggregate (`forward.summary`). */
export interface ForwardAgentSummary {
  agent_id: string;
  total: number;
  settled: number;
  avg_brier: number | null;
  avg_composite_error: number | null;
  /** Settled rows by error category (negligible/moderate/significant/critical). */
  categories: Record<string, number>;
  /** Settled rows by observation fidelity (full/mcp_only/none). */
  fidelity: Record<string, number>;
  /** All rows by prediction source tier. */
  sources: Record<string, number>;
  last_settled_at: string | null;
}

/** One prediction row (`forward.recent`), newest first. */
export interface ForwardPredictionRow {
  prediction_id: string;
  task_id: string;
  agent_id: string;
  round: number;
  source: string;
  fidelity: string | null;
  category: string | null;
  brier: number | null;
  composite_error: number | null;
  created_at: string;
  settled_at: string | null;
  /** Compact predicted-vs-actual pair for the list view (v1.59). */
  expected_outcome?: string | null;
  observed_outcome?: string | null;
  expected_artifact?: string | null;
  observed_artifact?: string | null;
  /** Task board title resolved server-side; null when the task is gone. */
  task_title?: string | null;
}

/** Expected side of one forward-chain round (parsed prediction). */
export interface ForwardChainExpected {
  tool_classes: string[];
  call_band: [number, number];
  outcome: string;
  artifact: string;
  confidence: number;
}

/** Observed side of one forward-chain round (parsed observation). */
export interface ForwardChainObserved {
  tool_classes: string[];
  calls: number;
  errors: number;
  outcome: string;
  artifact: string;
  fidelity: string;
  window_start: string;
  window_end: string;
}

/** One round of a task's predict→act→observe→score loop (`forward.chain`). */
export interface ForwardChainRound {
  prediction_id: string;
  task_id: string;
  agent_id: string;
  round: number;
  source: string;
  state_key: string;
  created_at: string;
  settled_at: string | null;
  expected: ForwardChainExpected | null;
  observed: ForwardChainObserved | null;
  fidelity: string | null;
  category: string | null;
  composite_error: number | null;
  brier: number | null;
  /** Which dimension the prediction missed on — null for legacy rows. */
  error_breakdown: {
    tool_set_error: number;
    volume_error: number;
    outcome_error: number;
    outcome_error_applicable: boolean;
    artifact_error: number;
  } | null;
}

/** Query-time calibration verdict for one agent (`forward.calibration`). */
export interface ForwardCalibration {
  agent_id: string;
  n: number;
  hit_rate: number | null;
  avg_brier: number | null;
  brier_skill_score: number | null;
  reliability: number | null;
  resolution: number | null;
  uncertainty: number | null;
  bins: Array<{ p_mean: number; emp_rate: number; n: number }>;
  /** supported | candidate | indistinguishable_from_luck */
  label: string;
}

/** One learned state bucket (`forward.states`), most-sampled first. */
export interface ForwardStateRow {
  state_key: string;
  agent_id: string;
  n_samples: number;
  last_updated: string;
}

/** One belief entry (`belief.recent`) — external-world prediction
 *  bookkeeping, parallel to the task forward model above. */
export interface BeliefRow {
  belief_id: string;
  agent_id: string;
  subject: string;
  horizon: string;
  /** up | down | flat */
  direction: string;
  prob: number;
  rationale: string | null;
  ref_value: number | null;
  predicted_at: string;
  stats_injected: boolean;
  realized_value: number | null;
  realized_direction: string | null;
  /** hit | miss | flat_band | null (unsettled) */
  outcome: string | null;
  brier: number | null;
  settled_at: string | null;
  settle_source: string | null;
  source_goal_id: string | null;
}

/** Per-subject rollup inside `BeliefStats.per_subject`. */
export interface BeliefSubjectStat {
  subject: string;
  n_settled: number;
  hits: number;
  mean_brier: number | null;
}

/** Per-agent belief calibration stats (`belief.summary`). n<30
 *  settled ⇒ `insufficient_samples: true` and every derived field is null
 *  (§0-3 small-sample discipline — never dress up a point estimate). */
export interface BeliefStats {
  agent_id: string;
  n_total: number;
  n_settled: number;
  insufficient_samples: boolean;
  hit_rate: number | null;
  hit_rate_wilson_low: number | null;
  mean_brier: number | null;
  overconfidence: number | null;
  per_subject: BeliefSubjectStat[];
}

// ── Security Audit (secaudit dashboard, DESIGN-code-security-audit-2026-08
// §3.1) — reports written by `duduclaw secaudit --save`. Field names mirror
// `crates/duduclaw-cli/src/secaudit/schema.rs`'s snake_case JSON contract
// verbatim; the gateway passes the report through as JSON rather than
// re-typing it (see `secaudit_reports.rs`'s doc comment), so these
// interfaces are the dashboard's own mirror of that same contract. */

/** critical | high | medium | low | info counts (`Summary.by_severity`). */
export interface SecauditSeverityCounts {
  critical: number;
  high: number;
  medium: number;
  low: number;
  info: number;
}

/** One row of `secaudit.reports` — shallow summary only, tolerant of a
 *  broken file (`parse_error` set, every other field `null`/absent). */
export interface SecauditReportRow {
  file: string;
  /** RFC3339, filesystem mtime. */
  mtime: string;
  repo: string | null;
  started_at: string | null;
  /** quick | deep */
  profile_mode: string | null;
  total_findings: number | null;
  by_severity: SecauditSeverityCounts | null;
  engines_run_count: number | null;
  engines_missing_count: number | null;
  parse_error: string | null;
}

/** One piece of a finding's evidence chain
 *  (static_hit | ai_analysis | adversarial_review | poc_transcript). */
export interface SecauditEvidenceItem {
  kind: string;
  source: string;
  detail: string;
  recorded_at: string;
}

/** candidate | confirmed | refuted | needs_human | suppressed. Only the
 *  first three of "confirmed/refuted/suppressed" are operator-writable via
 *  `secaudit.finding_status`. */
export type SecauditFindingStatus = 'confirmed' | 'suppressed' | 'refuted';

export interface SecauditFinding {
  id: string;
  source_engine: string;
  /** secret | static_analysis | dependency_vulnerability | other */
  kind: string;
  /** critical | high | medium | low | info */
  severity: string;
  title: string;
  file: string;
  line: number | null;
  snippet: string;
  rule_id: string;
  evidence: SecauditEvidenceItem[];
  /** candidate | confirmed | refuted | needs_human | suppressed */
  status: string;
}

export interface SecauditEngineRun {
  engine: string;
  findings_count: number;
  duration_ms: number;
  parse_error: string | null;
  timed_out: boolean;
}

export interface SecauditEngineMissing {
  engine: string;
  reason: string;
}

/** Full `AuditReport` JSON (`secaudit.report`). */
export interface SecauditReport {
  repo: string;
  started_at: string;
  profile: { mode: string; intake: unknown | null };
  engines_run: SecauditEngineRun[];
  engines_missing: SecauditEngineMissing[];
  findings: SecauditFinding[];
  summary: {
    total_findings: number;
    by_severity: SecauditSeverityCounts;
    engines_run_count: number;
    engines_missing_count: number;
    /** AI deep-audit + PoC counters (§3.2 steps 3-5) — additive fields, may
     *  be absent on a report written before those waves shipped. */
    ai_audit_candidates?: number;
    ai_audit_refuted?: number;
    ai_audit_needs_human?: number;
    poc_ran?: number;
  };
}

/** One recorded file effect from a task's rounds (`tasks.changes`).
 *  `path` carries the touched file — except for `op: 'shell'`, where it is the
 *  command itself (we never guess which files a shell line touched). */
export interface TaskChange {
  path: string;
  op: 'write' | 'edit' | 'delete' | 'shell';
  tool_name: string;
  timestamp: string;
  success: boolean;
  /** Masked excerpt produced by the audit layer; never re-read from disk. */
  snippet: string | null;
  source: 'native' | 'mcp_audit';
  round: number | null;
}

/** `tasks.changes` — the evidence behind the needs_human 「變更」tab. */
export interface TaskChanges {
  changes: TaskChange[];
  distinct_paths: number;
  truncated: boolean;
}

/** How a file in `attachments/` came to be there (I-2b provenance).
 *  · `declared` — the AI staff member handed it over explicitly
 *  · `swept`    — it produced the file but forgot to declare it; we recovered it
 *  · `produced` — a round wrote it, but no archived copy exists to download
 *  · `uploaded` — a human sent it in
 *  · `unknown`  — backfilled and the evidence could not place it（來源不明） */
export type ArtifactOrigin = 'declared' | 'swept' | 'produced' | 'uploaded' | 'unknown';

/** One deliverable of a task (`tasks.artifacts`). `attribution: 'inferred'`
 *  means only the time window ties it to this task — never presented as fact. */
export interface TaskArtifact {
  name: string;
  /** `/api/files/download?name=` key. `null` ⇒ written but never archived. */
  archived_name: string | null;
  agent_id: string;
  origin: ArtifactOrigin;
  attribution: 'exact' | 'inferred';
  produced_at: string;
  size: number | null;
  round: number | null;
  channel: string | null;
  source_path: string | null;
}

/** `tasks.artifacts` — what the task actually produced, for the 「產物」tab. */
export interface TaskArtifacts {
  artifacts: TaskArtifact[];
  truncated: boolean;
  /** How many rows are placed by the time window rather than by an id. */
  inferred_count: number;
}

/** `tasks.timeline` — one goal task's whole loop story. */
export interface GoalTimeline {
  task: TaskInfo;
  iterations: TaskIteration[];
  activity: ActivityEvent[];
  pending_kickoff: {
    id: string;
    summary: string;
    created_at: string;
    ttl_seconds: number;
  } | null;
  /** Execution transcripts recorded for this task's rounds (durable
   *  dispatch_runs linkage). */
  runs: Array<{
    id: string;
    round: number | null;
    status: string;
    started_at: string;
    ended_at: string;
    step_count: number;
  }>;
}

export interface EvolutionVersion {
  version_id: string;
  agent_id: string;
  soul_summary: string;
  soul_hash: string;
  applied_at: string;
  observation_end: string;
  status: string;
  /** WP0.4: was the one-time "insufficient observation data" alert already
   *  sent for this version? Only meaningful when `status === 'ExpiredNoData'`. */
  low_data_alert_sent?: boolean;
  pre_metrics: EvolutionMetrics;
  post_metrics: EvolutionMetrics | null;
}

/** One AVO §2.4 stagnation signal — `kind` selects which of the optional
 *  numeric fields are populated (see `gvu::stagnation::StagnationSignal`). */
export interface EvolutionStagnationSignal {
  kind: 'consecutive_non_applied' | 'zero_apply_window' | 'repeated_rejection_reason';
  count?: number;
  threshold?: number;
  days?: number;
  trigger_count?: number;
  occurrences?: number;
  reason_prefix?: string;
}

export interface EvolutionStagnationSnapshot {
  agent_id: string;
  is_stagnant: boolean;
  signals: EvolutionStagnationSignal[];
  /** zh-TW human summary from the backend — kept for debugging/logs only;
   *  the UI renders `signals` through i18n instead so all three locales agree. */
  summary: string | null;
  checked_at: string;
}

export interface EvolutionTelemetrySummary {
  agent_id: string;
  days: number;
  total: number;
  /** stage ("verify" | "apply") -> layer/gate name -> rejection count. */
  by_stage_layer: Record<string, Record<string, number>>;
}

export interface EvolutionConsolidation {
  id: string;
  agent_id: string;
  attempted_at: string;
  outcome: string;
  from_bytes: number;
  to_bytes: number | null;
  detail: string | null;
}

/** §C.9 outward status vocabulary for an experience rule (經驗法則). */
export type RuleStatusKey = 'observing' | 'trial' | 'active' | 'dormant' | 'retired';

/**
 * W3-2 — the gateway's zero-LLM plain-language rewrite of a rule.
 *
 * `sentence` is ready-to-show zh-TW; the `*_key` fields are stable machine
 * keys so the UI localizes rather than echoing the server's zh-TW label.
 * `fallback === true` means the templates could not produce a sentence and
 * `sentence` is the raw stored text — render it as such, never as a rewrite.
 */
export interface HumanizedRule {
  sentence: string;
  condition: string;
  action: string;
  purpose: string;
  purpose_key: PlaybookEntry['category'];
  status: string;
  status_key: RuleStatusKey;
  why: string;
  fallback: boolean;
  evidence: {
    eval_cases: number;
    failure_notes: number;
    applications: number;
    helpful: number;
    harmful: number;
    success_streak: number;
  };
}

export interface PlaybookEntry {
  id: string;
  content: string;
  category: 'repair' | 'optimize' | 'innovate' | 'regulatory' | 'explore';
  state: 'probation' | 'active' | 'stale' | 'retired';
  signals_match: string[];
  eval_cases: string[];
  success_streak: number;
  revision: number;
  helpful: number;
  harmful: number;
  net_score: number;
  origin: string;
  created_at: string;
  /** Optional so an older gateway (pre-W3-2) still type-checks. */
  humanized?: HumanizedRule;
}

export interface BrowserAuditEntry {
  id: string;
  timestamp: string;
  agent_id: string;
  action: string;
  url?: string;
  screenshot?: string;
  screenshot_path?: string;
  tier?: string;
  domain?: string;
  risk_level: 'low' | 'medium' | 'high';
  details: Record<string, unknown>;
}

export interface BrowserbaseSession {
  session_id: string;
  agent_id: string;
  status: 'active' | 'closed' | 'error' | 'running' | 'completed';
  created_at: string;
  url?: string;
  replay_url?: string;
}

export interface BrowserbaseCostSummary {
  total_sessions: number;
  active_sessions: number;
  estimated_cost_cents: number;
  total_cost_usd?: number;
  total_duration_seconds?: number;
  hours: number;
}

export interface ToolApproval {
  tool_name: string;
  agent_id: string;
  approved_at: string;
  expires_at?: string;
  session_scoped: boolean;
  duration_minutes?: number;
}

export interface BillingUsageMeter {
  used: number;
  limit: number;
}

export interface BillingUsage {
  plan: string;
  tier: string;
  conversations: BillingUsageMeter;
  agents: BillingUsageMeter;
  channels: BillingUsageMeter;
  inference_hours: BillingUsageMeter;
  reset_at: string;
}

export interface BillingInvoice {
  id: string;
  date: string;
  amount_cents: number;
  status: 'paid' | 'pending' | 'failed';
  description: string;
  pdf_url?: string;
}

// ── Partner Portal types ───────────────────────────────────

export interface PartnerProfile {
  company: string | null;
  tier: string;
  partner_id: string | null;
  certified_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface PartnerStats {
  total_sold: number;
  active_customers: number;
  this_month_commission_cents: number;
  lifetime_commission_cents: number;
}

export interface PartnerCustomer {
  id: string;
  name: string;
  tier: string;
  activated_at: string;
  status: string;
  commission_cents: number;
  notes: string | null;
  created_at: string;
}

/**
 * License snapshot returned by `license.status` RPC.
 *
 * Shape mirrors `crate::license_runtime::LicenseSnapshot` exactly — adjust
 * here and in the Rust struct in lockstep when extending. The snapshot
 * deliberately omits the raw Ed25519 signature; the dashboard never needs
 * it and serializing it would only invite copy-paste leaks.
 */
export interface LicenseSnapshot {
  /** Active tier — `opensource` when no license is installed. */
  tier:
    | 'opensource'
    | 'hobby'
    | 'solo'
    | 'studio'
    | 'business'
    | 'partner'
    | 'personal_pro_self_host'
    | 'self_host_pro'
    | 'oem';
  /** Always one of two stable strings — useful for UI conditionals. */
  mode: 'opensource' | 'commercial';
  /** False when no license.json exists; true otherwise. */
  installed: boolean;
  customer_id?: string | null;
  subscription_id?: string | null;
  /** RFC3339 timestamp. */
  expires_at?: string | null;
  /** Negative when already expired. */
  days_until_expiry?: number | null;
  /** RFC3339 timestamp of last successful phone-home. */
  last_phone_home?: string | null;
  days_since_phone_home?: number | null;
  /** `true` when the license fingerprint matches the current machine. */
  fingerprint_match?: boolean | null;
  /**
   * NFR (Not-For-Resale) internal-test license. Renders a badge that
   * white-label branding must never remove — the anti-resale watermark.
   */
  nfr?: boolean;
}

// ── User management types ────────────────────────────────────

export interface UserInfo {
  id: string;
  email: string;
  display_name: string;
  role: 'admin' | 'manager' | 'employee';
  status: 'active' | 'suspended' | 'offboarded';
  /** Department this user belongs to (drives install-approval routing). */
  department?: string | null;
  created_at: string;
  updated_at: string;
  last_login?: string;
}

export interface UserAgentBinding {
  user_id: string;
  agent_name: string;
  access_level: 'owner' | 'operator' | 'viewer';
  bound_at: string;
}

export interface UserDetail extends UserInfo {
  bindings: UserAgentBinding[];
}

export interface AuditEntry {
  id: number;
  user_id?: string;
  action: string;
  target?: string;
  detail?: string;
  ip?: string;
  timestamp: string;
}

export interface OdooStatus {
  connected: boolean;
  edition?: string;
  version?: string;
  uid?: number;
  error?: string;
}

/** One model row from `odoo.discover_schema` (metadata only). */
export interface OdooSchemaModel {
  model: string;
  name: string;
  custom: boolean;
  field_count: number;
}

/** Result of an `odoo.discover_schema` scan. */
export interface OdooDiscoverSchemaResult {
  success: boolean;
  message?: string;
  models?: OdooSchemaModel[];
  total_models?: number;
  truncated?: boolean;
  wiki_written?: boolean;
  wiki_note?: string;
}

export interface OdooConfig {
  url: string;
  db: string;
  protocol: string;
  auth_method: string;
  username: string;
  poll_enabled: boolean;
  poll_interval_seconds: number;
  poll_models: string[];
  webhook_enabled: boolean;
  /**
   * Whether each write-only credential is on file. The values stay server-side;
   * these flags are what let the form say "已儲存（留空表示不變更）" instead of
   * showing the same dots whether or not anything was ever saved.
   */
  has_api_key?: boolean;
  has_password?: boolean;
  has_webhook_secret?: boolean;
  unblock_models?: string[];
  features_crm: boolean;
  features_sale: boolean;
  features_inventory: boolean;
  features_accounting: boolean;
  features_project: boolean;
  features_hr: boolean;
}

export interface OdooConfigUpdate {
  url: string;
  db: string;
  protocol: string;
  auth_method: string;
  username: string;
  api_key?: string;
  password?: string;
  poll_enabled: boolean;
  poll_interval_seconds: number;
  poll_models: string[];
  webhook_enabled: boolean;
  webhook_secret?: string;
  unblock_models?: string[];
  features_crm: boolean;
  features_sale: boolean;
  features_inventory: boolean;
  features_accounting: boolean;
  features_project: boolean;
  features_hr: boolean;
}

/** Optional inline params for `odoo.test` — when provided, the backend
 *  tests with these values instead of the saved config.toml.
 *  Omit credentials to fall back to the stored API key / password. */
export interface OdooTestParams {
  url: string;
  db: string;
  protocol: string;
  auth_method: string;
  username: string;
  api_key?: string;
  password?: string;
}

export interface McpServerDef {
  command: string;
  args: string[];
  env: Record<string, string>;
}

export interface McpServerEntry extends McpServerDef {
  name: string;
}

export interface McpAgentConfig {
  agent_id: string;
  /** The backend serializes servers as an array of named entries. */
  servers: McpServerEntry[];
}

export interface McpCatalogItem {
  id: string;
  name: string;
  description: string;
  category: string;
  /** Author is always populated by the backend; optional here for
   *  backward compatibility with any older JSON payloads. */
  author?: string;
  tags?: string[];
  featured?: boolean;
  requires_oauth: boolean;
  default_def: McpServerDef;
  required_env: string[];
}

export interface McpScanFinding {
  category: string;
  severity: string;
  description: string;
  pattern?: string;
}

export interface McpScanResult {
  passed: boolean;
  risk_level: string;
  findings: McpScanFinding[];
}

export interface McpImportCandidate extends McpServerDef {
  name: string;
  description: string;
  scan: McpScanResult;
  passed: boolean;
}

export interface McpImportFetchResult {
  source_url: string;
  resolved_url: string;
  servers: McpImportCandidate[];
}

// ── Install approval requests (Skill / MCP two-stage signature chain) ──

export interface InstallRequestScanFinding {
  category: string;
  severity: string;
  description: string;
  pattern?: string;
}

export type InstallRequestStage =
  | 'awaiting_manager'
  | 'awaiting_admin'
  | 'approved'
  | 'denied'
  | 'expired';

export interface InstallRequestInfo {
  id: string;
  kind: 'skill' | 'mcp';
  title: string;
  description: string;
  requester_id: string;
  requester_email: string;
  requester_role: 'employee' | 'manager' | 'admin';
  requester_department?: string | null;
  risk_level: string;
  scan: InstallRequestScanFinding[];
  status: 'pending' | 'approved' | 'denied' | 'expired';
  stage: InstallRequestStage;
  manager_by: string | null;
  admin_by: string | null;
  decided_reason: string | null;
  executed: boolean;
  execute_error: string | null;
  created_at: string;
  ttl_seconds: number;
  /** Unix epoch (seconds) this request auto-expires at. `null` on an
   *  unparseable `created_at` — treat as "no countdown available". */
  expires_at?: number | null;
  // ── E8 reverse handoff, extended to install requests ─────────
  /** Originating/reachable channel for this request's sign-off
   *  conversation (e.g. `telegram`), when one could be resolved. `null`/
   *  absent when no card was ever recorded and no current-stage approver
   *  has a linked channel. */
  channel?: string | null;
  /** Server-resolved "open in `channel`" URL, or `null` when nothing could
   *  be constructed for this platform (never a raw chat/message id — see
   *  `channel_link.rs`). Render the button ONLY when this is a non-empty
   *  string. */
  channel_link?: string | null;
}

export interface InstallRequestFiled {
  request_id: string;
  status: 'pending';
  stage: InstallRequestStage;
  scan: InstallRequestScanFinding[];
}

export interface MarketplaceServer {
  id: string;
  name: string;
  description: string;
  category: string;
  author: string;
  tags: string[];
  featured: boolean;
  requires_oauth: boolean;
  required_env: string[];
  /** Agent ids that already have this server in their `.mcp.json` (backend-derived). */
  installed_by: string[];
}

export interface McpOAuthProvider {
  provider_id: string;
  /** Display name. Absent on older gateways — fall back to `provider_id`. */
  name?: string;
  auth_url: string;
  scopes: string[];
  configured: boolean;
  token_status: 'none' | 'authenticated' | 'expired';
  /** Legacy alias of `token_status` (older gateways sent only this one). */
  status?: 'none' | 'authenticated' | 'expired';
  expires_at: string | null;
  /**
   * The saved OAuth client id, in full. Client ids are public by design (they
   * travel in the authorization URL), and showing the real value is the whole
   * point: a field left on its placeholder is what makes a working integration
   * look like it lost its settings.
   */
  client_id?: string;
  /** Whether a client secret is on file. The value itself never leaves the gateway. */
  has_client_secret?: boolean;
  /** Tail-masked hint for the stored secret, e.g. `••••9f3c`. */
  client_secret_masked?: string;
  /** A refresh token is on file — an expired access token renews itself. */
  can_refresh?: boolean;
  /** Whether the stored access token is still within its lifetime. */
  access_token_valid?: boolean;
  /**
   * The exact redirect URI to register in the provider's console, derived
   * server-side from the live gateway port. Absent on older gateways — the UI
   * falls back to its built-in default, which is only correct on the default
   * port.
   */
  redirect_uri?: string;
}

/** Fields that can be updated on an agent via `agents.update`. All optional. */
export interface AgentUpdateParams {
  // Identity
  display_name?: string;
  role?: string;
  status?: string;
  trigger?: string;
  icon?: string;
  reports_to?: string;
  /** WP7 — department (ASCII alphanumeric + `-`/`_`, 1..=64). Empty string
   *  clears it (the agent leaves its department). Admin-only server-side. */
  department?: string;
  // Model
  preferred?: string;
  fallback?: string;
  api_mode?: 'cli' | 'direct' | 'auto';
  // Budget
  monthly_limit_cents?: number;
  warn_threshold_percent?: number;
  hard_stop?: boolean;
  // Heartbeat
  heartbeat_enabled?: boolean;
  heartbeat_interval?: number;
  heartbeat_cron?: string;
  // Permissions
  can_create_agents?: boolean;
  can_send_cross_agent?: boolean;
  can_modify_own_skills?: boolean;
  can_modify_own_soul?: boolean;
  can_schedule_tasks?: boolean;
  // Local model
  local_model?: string;
  local_backend?: string;
  local_context_length?: number;
  local_gpu_layers?: number;
  prefer_local?: boolean;
  use_router?: boolean;
  // Container
  timeout_ms?: number;
  max_concurrent?: number;
  sandbox_enabled?: boolean;
  network_access?: boolean;
  readonly_project?: boolean;
  // Evolution
  skill_auto_activate?: boolean;
  skill_security_scan?: boolean;
  gvu_enabled?: boolean;
  cognitive_memory?: boolean;
  max_active_skills?: number;
  max_silence_hours?: number;
  max_gvu_generations?: number;
  observation_period_hours?: number;
  skill_token_budget?: number;
  // Proactive ([proactive] section, nested object). Includes G.8 extras
  // (token_budget_per_check / timezone / max_turns) accepted by the backend.
  proactive?: Partial<ProactiveSettings> & {
    token_budget_per_check?: number;
    timezone?: string;
    max_turns?: number;
  };
  // Per-agent channels
  discord_bot_token?: string;
  telegram_bot_token?: string;
  line_channel_token?: string;
  line_channel_secret?: string;
  slack_app_token?: string;
  slack_bot_token?: string;
  whatsapp_access_token?: string;
  whatsapp_verify_token?: string;
  whatsapp_phone_number_id?: string;
  whatsapp_app_secret?: string;
  feishu_app_id?: string;
  feishu_app_secret?: string;
  feishu_verification_token?: string;
  // Sticker
  sticker_enabled?: boolean;
  sticker_probability?: number;
  sticker_intensity_threshold?: number;
  sticker_cooldown_messages?: number;
  sticker_expressiveness?: 'minimal' | 'moderate' | 'expressive';
  // Capabilities ([capabilities] section, nested object)
  capabilities?: AgentCapabilities;
  // v1.39 — OS-native filesystem watch ([os_watch] top-level table)
  os_watch?: OsWatchConfig;
  // Belief loop × goal contract gap 2 — self-study opt-in ([research] table)
  research?: {
    self_study?: boolean;
    /** Local wall-clock hour (0-23) past which the goal may fire. */
    self_study_hour?: number;
  };
  // RT — Runtime ([runtime] section, nested object)
  runtime?: AgentRuntime;
  // EVO — advanced evolution ([evolution.*] fields, nested object)
  evolution_advanced?: AgentEvolutionAdvanced;
  // CT — advanced container ([container.*] fields, nested object)
  container_advanced?: AgentContainerAdvanced;
  // ODO — per-agent [odoo] override (nested object). api_key/password write-only.
  odoo?: AgentOdooOverride;
  // G.8 — [model] extras
  account_pool?: string[];
  utility?: string;
  // G.8 — [heartbeat] extras
  heartbeat_max_concurrent_runs?: number;
  heartbeat_cron_timezone?: string;
  // UI.3 — stagnation detection ([evolution.stagnation_detection])
  stagnation_enabled?: boolean;
  stagnation_window_seconds?: number;
  stagnation_trigger_threshold?: number;
  stagnation_action?: 'log_only' | 'suppress';
  // G.8 — free-form scalar tables
  ptc?: Record<string, string | number | boolean>;
  prompt?: Record<string, string | number | boolean>;
  cultural_context?: Record<string, string | number | boolean>;
}

// ── WP4: agent handoff (offboard with transfer) ─────────────────

export interface AgentHandoffParams {
  from_agent: string;
  to_agent: string;
  /** Move episodic + semantic memory + key facts. Default true. */
  memory?: boolean;
  /** Move the agent's private wiki pages. Default true. */
  wiki?: boolean;
  /** Reassign open tasks. Default true. */
  tasks?: boolean;
  /** Archive the source agent once the transfer completes. Default true. */
  auto_archive?: boolean;
}

/** Result of `agents.handoff`. `status` is COMPLETE only when every requested
 *  sub-move succeeded; otherwise PARTIAL with `errors[]` populated and
 *  `success: false`. Each sub-object is present only when its move was requested. */
export interface AgentHandoffResult {
  success: boolean;
  status: 'COMPLETE' | 'PARTIAL';
  from_agent: string;
  to_agent: string;
  memory?: { moved?: number; memories?: number; key_facts?: number; archived_rows?: number; error?: string };
  wiki?: { files_moved?: number; error?: string };
  tasks?: { reassigned?: number; error?: string };
  auto_archive?: { archived?: boolean; skipped?: string; error?: string };
  errors?: string[];
}

// ── ODO: per-agent [odoo] override ──────────────────────────────

/** The `odoo` object accepted by `agents.update`. All fields optional —
 *  the backend only writes fields that are present (partial update).
 *  `api_key` / `password` are WRITE-ONLY: never returned, only sent when
 *  the operator types a new value (sending the masked placeholder is a no-op). */
export interface AgentOdooOverride {
  profile?: string;
  allowed_models?: string[];
  /** Bare verb (read/write/create/unlink/execute) or `verb:model` (e.g. write:crm.lead). */
  allowed_actions?: string[];
  company_ids?: number[];
  url?: string;
  db?: string;
  username?: string;
  /** Write-only — encrypted server-side. */
  api_key?: string;
  /** Write-only — encrypted server-side. */
  password?: string;
}

// ── RT: per-agent [runtime] ─────────────────────────────────────

export type RuntimeProvider = 'claude' | 'codex' | 'gemini' | 'antigravity' | 'grok' | 'openai_compat';

/** The `runtime` object accepted by `agents.update`. All fields optional —
 *  the backend only writes fields that are present. An empty `fallback`
 *  string clears the fallback. */
export interface AgentRuntime {
  provider?: RuntimeProvider;
  /** A provider name, or '' to clear. Must be a valid provider when non-empty. */
  fallback?: string;
  pty_pool_enabled?: boolean;
  worker_managed?: boolean;
}

/** Result of `runtime.detect` — which AI backends are installed + Claude OAuth. */
export interface RuntimeDetect {
  claude_cli: boolean;
  codex: boolean;
  gemini: boolean;
  antigravity: boolean;
  /** Present since the R4 Grok runtime; older gateways omit it. */
  grok?: boolean;
  claude_oauth: boolean;
  claude_subscription: string | null;
}

/** Providers `runtime.install` knows how to install (hard-coded gateway whitelist). */
export type InstallableProvider = 'claude' | 'codex' | 'gemini' | 'antigravity' | 'grok';

/**
 * Result of `runtime.install`. A decline is a normal answer, not an error: the
 * gateway hands back the exact command to paste so the user is never stuck.
 */
export type RuntimeInstallStart =
  | {
      started: true;
      provider: InstallableProvider;
      session_id: string;
      command: string;
      docs_url: string;
      timeout_secs: number;
    }
  | {
      started: false;
      provider: InstallableProvider;
      /** `already_installed` | `already_running` | `prerequisite_missing`
       *  | `posix_script_on_windows` | `install_target_not_on_probe_path`
       *  | `spawn_failed` */
      reason: string;
      command: string;
      prerequisite: string | null;
      docs_url: string;
    };

/** `runtime.install.output` event — one line of installer output. */
export interface RuntimeInstallOutput {
  session_id: string;
  provider: InstallableProvider;
  data: string;
}

/** `runtime.install.status` event — terminal result of one install run. */
export interface RuntimeInstallStatus {
  session_id: string;
  provider: InstallableProvider;
  status: 'succeeded' | 'failed' | 'timeout';
  exit_code: number | null;
  /** Authoritative: is the binary actually resolvable now? */
  detected: boolean;
  command: string;
  docs_url: string;
}

// ── Expert packs (專家包) ─────────────────────────

/** One installed expert pack row from `experts.list` (admin-only). */
export interface ExpertPack {
  slug: string;
  /** Source format label (internal; UI shows a friendly badge instead). */
  kind: string;
  display_name: string;
  version: string;
  description: string;
  /** AI staffer ids created by this pack. */
  agents: string[];
  skills_count: number;
  wiki_count: number;
  installed_at: string;
  /** null ⇒ the pack ships no managed hooks. */
  hooks_status: 'disabled' | 'pending_approval' | 'enabled' | null;
  hooks_files: number;
}

/** One AI-team roster member (`experts.catalog` `members[]`, team entries
 *  only) — display_name/summary verbatim from `team.toml`, never re-authored
 *  client-side. */
export interface ExpertCatalogMember {
  role: 'front_desk' | 'worker';
  name: string;
  display_name: string;
  summary: string;
}

/** A position deliberately left to a human (`experts.catalog` `humans[]`). */
export interface ExpertCatalogHuman {
  title: string;
  summary: string;
}

/** A shared worker kit deliberately not deployed for this team
 *  (`experts.catalog` `excluded[]`). */
export interface ExpertCatalogExcludedKit {
  kit: string;
  reason: string;
}

/** One built-in industry pack row from `experts.catalog` (admin-only). */
export interface ExpertCatalogEntry {
  /** WP-ORG — `team` = industry team pack; `expert` = standalone expert pack. */
  kind?: 'team' | 'expert';
  /** Present on team entries only. */
  industry?: string;
  /** Catalog section slug (health/professional/retail/lifestyle/education/other). */
  category?: string;
  /** Distinct functional departments the roster lands in (zh-TW data strings). */
  departments?: string[];
  label: string;
  slug: string;
  description: string;
  agents_count: number;
  installed: boolean;
  /** P2-a — team roster (front desk + workers). Team entries only; standalone
   *  `expert` entries carry no per-member summary in their manifest. */
  members?: ExpertCatalogMember[];
  /** P2-a — positions kept human (team entries only). */
  humans?: ExpertCatalogHuman[];
  /** P2-a — shared kits deliberately excluded from this team (team entries only). */
  excluded?: ExpertCatalogExcludedKit[];
  /** P2-a — 2-3 concrete task examples; author-written in `team.toml`, or
   *  derived from real worker summaries when none are authored (never
   *  LLM-fabricated). */
  examples?: string[];
  /** P2-a — once installed, the agent id the "已加入" entry point should link
   *  into (`/agents/<name>`). `null`/absent when not installed, or when the
   *  install record has no resolvable agent. */
  lead_agent_name?: string | null;
}

// ── Inspiration gallery (靈感畫廊, P2-b, curated-only MVP) ───────────

/** One showcase card from `gallery.list` — a single team task example,
 *  fanned out from the same `team.toml` data `experts.catalog` reads (no
 *  new storage, nothing user-submitted in this wave). */
/** Agent Mail (P2-d) — one arrived message, as `mail.list` returns it. */
export interface MailMessage {
  mail_id: string;
  agent_id: string;
  from: string;
  subject: string;
  /** List view only: first ~160 chars. `mail.read` returns the full `body`. */
  snippet: string;
  received_at: string;
  /** Which transport delivered it (`gmail` / `dropfolder`). */
  source: string;
  read: boolean;
  archived: boolean;
  /** An AI staff member was woken for this mail (到達即觸發). */
  handled: boolean;
  /** The prompt-injection scanner flagged the content. Shown, never hidden. */
  flagged: boolean;
  risk_score: number;
}

/** Full message body, as `mail.read` returns it (and marks the mail read). */
export interface MailMessageFull extends Omit<MailMessage, 'snippet'> {
  body: string;
}

/** An outgoing draft awaiting (or past) human confirmation. */
export interface MailDraft {
  mail_id: string;
  agent_id: string;
  to: string;
  subject: string;
  body: string;
  created_at: string;
  /** `pending` is the only state in which nothing has left the building. */
  status: 'pending' | 'sent' | 'rejected' | 'failed';
  approval_id: string;
  in_reply_to?: string | null;
  /** Why it was rejected, or the transport error behind a `failed`. */
  note?: string | null;
  settled_at?: string | null;
}

/** Mailbox feature state — enough for the page to explain itself, never any
 *  credential (only whether sending is possible at all). */
export interface MailStatus {
  enabled: boolean;
  auto_trigger: boolean;
  gmail_enabled: boolean;
  dropfolder_enabled: boolean;
  poll_interval_secs: number;
  default_agent: string;
  smtp_configured: boolean;
  sender_allowlist_count: number;
  recipient_allowlist_count: number;
  inbound_dir: string;
}

/** WP-7I — I-5 ⌘K content search. One cross-source hit from `search.query`.
 *  `jump` is intentionally loosely typed — its shape depends on `source`
 *  (matches the gateway's `search_index::SearchHit`, which documents each
 *  source's jump-target fields). */
export interface SearchHit {
  source: 'conversation' | 'artifact' | 'memory' | 'wiki' | 'shared_wiki';
  id: string;
  title: string;
  snippet: string;
  agent_id?: string | null;
  timestamp?: string | null;
  jump: Record<string, unknown>;
}

/** WP-7I — one entry from `presets.list` (agent preset P1, read-only). A
 *  preset that failed to parse still appears, carrying `error` instead of the
 *  usual metadata — the CLI's `duduclaw preset list` never silently drops a
 *  broken preset, and this mirrors that. */
export interface PresetSummary {
  id: string;
  version?: string;
  label?: string;
  description?: string;
  error?: string;
}

/** WP-7I — one agent's live preset binding + resolution outcome, as
 *  `presets.status` returns it. `state: 'unbound'` carries no other field;
 *  `'applied'` carries the resolved preset's identity plus which of the
 *  agent's own fields override it; `'unresolved'` carries why resolution
 *  failed (fail-closed — the agent still runs on its own `agent.toml`). */
export interface PresetResolution {
  state: 'unbound' | 'applied' | 'unresolved';
  preset_id?: string;
  version?: string;
  label?: string;
  changed_fields?: string[];
  reason?: string;
}

// ── WP-C: appliance device management ("裝置" page) ─────────────────────

/** Machine-readable code the gateway returns for every `device.*` RPC
 *  reached on a non-appliance install (`handlers.rs::DEVICE_NOT_APPLIANCE_ERROR_CODE`). */
export const DEVICE_NOT_APPLIANCE_ERROR_CODE = 'not_appliance';
/** Returned when a destructive `device.*` RPC is called without `confirm: true`. */
export const DEVICE_CONFIRM_REQUIRED_ERROR_CODE = 'confirm_required';

export interface DeviceLoadAverage {
  load1: number;
  load5: number;
  load15: number;
}

export interface DeviceMemInfo {
  total_mb: number;
  available_mb: number;
  used_mb: number;
}

export interface DeviceDiskUsage {
  total_mb: number;
  used_mb: number;
  available_mb: number;
}

export interface DeviceNetworkInterface {
  name: string;
  is_up: boolean;
  addresses: string[];
}

/** `device.status` payload. Every field but `cpu_cores`/`network_interfaces`
 *  is `null` when the underlying sensor can't be read (off-appliance-Linux
 *  dev host, or hardware with no exposed sensor) — `null` is a normal, honest
 *  reading, not an error; the UI omits that row rather than showing 0/blank. */
export interface DeviceStatus {
  cpu_cores: number;
  load_average: DeviceLoadAverage | null;
  ram: DeviceMemInfo | null;
  disk: DeviceDiskUsage | null;
  temperature_c: number | null;
  uptime_secs: number | null;
  network_interfaces: DeviceNetworkInterface[];
}

/** Shape shared by every shell-out-backed `device.*` RPC
 *  (`update_status`/`update_apply`/`factory_reset`/`power`) — a normal `Ok`
 *  can still carry `success: false` (the command ran but exited non-zero). */
export interface DeviceOpResult {
  success: boolean;
  stdout: string;
  stderr: string;
}

export interface DeviceBackupResult {
  /** Pass straight to `GET /api/files/download?name=<filename>` to fetch it. */
  filename: string;
  stdout: string;
  stderr: string;
}

// ── WP-G1: scheduled backups + device-migration restore ────────────────

/** `device.backup_schedule_get` / `device.backup_schedule_set` payload. */
export interface DeviceBackupScheduleConfig {
  schedule_enabled: boolean;
  interval_hours: number;
  retention_count: number;
}

/** One row from `device.backup_list` — a file under the gateway's dedicated
 *  `<home>/backups/` directory (never the task/channel attachments dir). */
export interface DeviceBackupFileEntry {
  name: string;
  size: number;
  /** Unix epoch milliseconds. */
  mtime: number;
}

export interface DeviceBackupRestoreResult {
  staged: true;
  files_written: number;
  restart_required: true;
}

export interface GalleryCard {
  /** Deterministic (`<team-slug>-<example-index>`) — stable React key. */
  id: string;
  industry: string;
  /** Catalog section slug (health/professional/retail/lifestyle/education/other) —
   *  same values as `ExpertCatalogEntry.category`. */
  category: string;
  /** Distinct functional departments the team's roster lands in (zh-TW data strings). */
  departments: string[];
  team_slug: string;
  team_label: string;
  /** The task example itself — verbatim from `team.toml`, never rewritten. */
  example: string;
  team_installed: boolean;
  /** Once the team is installed, the agent id "做同款" should preselect. */
  lead_agent_name?: string | null;
}

/** Preview of an LLM-generated expert-pack draft (`experts.generate`). */
export interface ExpertDraftPreview {
  slug: string;
  display_name: string;
  description: string;
  version: string;
  prompts: string[];
  channels: string[];
  agents: Array<{
    name: string;
    role: string;
    display_name: string;
    reports_to: string;
    soul_excerpt: string;
  }>;
  skills: string[];
  wiki_titles: string[];
}

/** `experts.generate` / `experts.generate_revise` result envelope. */
export interface ExpertDraftResult {
  draft_id: string;
  rounds: number;
  rounds_left: number;
  preview: ExpertDraftPreview;
}

// ── EVO: per-agent advanced [evolution] ─────────────────────────

export interface EvolutionExternalFactors {
  user_feedback?: boolean;
  security_events?: boolean;
  channel_metrics?: boolean;
  business_context?: boolean;
  peer_signals?: boolean;
}

/** The `evolution_advanced` object accepted by `agents.update`. All fields
 *  optional. Thresholds are 0.0–1.0 floats; *_hours / *_daily / ttl are
 *  unsigned integers. */
export interface AgentEvolutionAdvanced {
  external_factors?: EvolutionExternalFactors;
  // Skill synthesis
  skill_synthesis_enabled?: boolean;
  skill_synthesis_threshold?: number;
  skill_synthesis_cooldown_hours?: number;
  skill_trial_ttl?: number;
  // Skill graduation
  skill_graduation_enabled?: boolean;
  skill_graduation_min_lift?: number;
  // Skill recommendation
  skill_recommendation_enabled?: boolean;
  skill_recommendation_threshold?: number;
  // Curiosity
  curiosity_enabled?: boolean;
  curiosity_threshold?: number;
  curiosity_max_daily?: number;
  // Behavior monitor
  skill_behavior_monitor_enabled?: boolean;
  skill_behavior_drift_threshold?: number;
}

// ── CT: per-agent advanced [container] ──────────────────────────

export interface ContainerMount {
  host: string;
  container: string;
  readonly: boolean;
}

export interface ContainerEnvVar {
  key: string;
  value: string;
}

/** The `container_advanced` object accepted by `agents.update`. All fields
 *  optional. Mount host paths matching the gateway blocked-pattern list
 *  (e.g. `.ssh`, `.env`) are rejected server-side. */
export interface AgentContainerAdvanced {
  worktree_enabled?: boolean;
  worktree_auto_merge?: boolean;
  worktree_cleanup_on_exit?: boolean;
  worktree_copy_files?: string[];
  additional_mounts?: ContainerMount[];
  cmd?: string[];
  env?: ContainerEnvVar[];
}

// ── INF: inference.toml ─────────────────────────────────────────

export interface InferenceGeneration {
  max_tokens?: number;
  temperature?: number;
  top_p?: number;
  stop?: string[];
  gpu_layers?: number;
  context_size?: number;
}

export interface InferenceRouter {
  enabled?: boolean;
  fast_threshold?: number;
  /** Must be < fast_threshold (validated server-side). */
  strong_threshold?: number;
  fast_model?: string;
  strong_model?: string;
  max_fast_prompt_tokens?: number;
  cloud_keywords?: string[];
  fast_keywords?: string[];
}

/** `[openai_compat]` — the api_key is WRITE-ONLY. On read, the gateway returns
 *  `api_key_set: bool` plus a masked placeholder in `api_key` ("***set***").
 *  Only send a new `api_key` on update when the operator types one. */
export interface InferenceOpenAiCompat {
  base_url?: string;
  model?: string;
  /** On read: masked placeholder. On write: cleartext (encrypted server-side),
   *  '' clears it. Never send back the masked placeholder. */
  api_key?: string;
  /** Read-only flag indicating a secret is stored. */
  api_key_set?: boolean;
}

/** Generic pass-through backend sections — flat tables of scalars/arrays. */
export type InferenceBackendSection = Record<string, unknown>;

/** Full inference.toml shape returned by `inference.get`. The openai_compat
 *  api_key is masked. Unknown sub-sections surface generically. */
export interface InferenceConfig {
  enabled?: boolean;
  backend?: string;
  models_dir?: string;
  default_model?: string;
  auto_load?: boolean;
  max_memory_mb?: number;
  generation?: InferenceGeneration;
  router?: InferenceRouter;
  openai_compat?: InferenceOpenAiCompat;
  exo?: InferenceBackendSection;
  llamafile?: InferenceBackendSection;
  mlx?: InferenceBackendSection;
  mistralrs?: InferenceBackendSection;
  llmlingua?: InferenceBackendSection;
  streaming_llm?: InferenceBackendSection;
  embedding?: InferenceBackendSection;
  [key: string]: unknown;
}

/** Partial update payload for `inference.update`. Omit a section to leave it
 *  untouched. For openai_compat, omit `api_key` to keep the stored secret. */
export interface InferenceUpdate {
  enabled?: boolean;
  backend?: string;
  models_dir?: string;
  default_model?: string;
  auto_load?: boolean;
  max_memory_mb?: number;
  generation?: InferenceGeneration;
  router?: InferenceRouter;
  openai_compat?: InferenceOpenAiCompat;
  exo?: InferenceBackendSection;
  llamafile?: InferenceBackendSection;
  mlx?: InferenceBackendSection;
  mistralrs?: InferenceBackendSection;
  llmlingua?: InferenceBackendSection;
  streaming_llm?: InferenceBackendSection;
  embedding?: InferenceBackendSection;
}

// ── IDR: [identity] identity resolution (RFC-21 §1) ─────────────

/** Which `duduclaw_identity` provider is active. */
export type IdentityProviderKind = 'wiki_cache' | 'notion' | 'chained';

export type IdentityProjectsKind = 'multi_select' | 'relation';

/** Maps DuDuClaw's logical fields onto Notion property names. */
export interface IdentityNotionFieldMap {
  name?: string;
  roles?: string;
  projects?: string;
  projects_kind?: IdentityProjectsKind;
  emails?: string;
  /** channel-wire-name → Notion property name. */
  channel_props?: Record<string, string>;
}

/** `[identity.notion]` — the api_key is WRITE-ONLY. On read the gateway returns
 *  `api_key_set: bool` plus a masked placeholder in `api_key` ("***set***"). */
export interface IdentityNotionConfig {
  database_id?: string;
  refresh_seconds?: number;
  /** On read: masked placeholder. On write: cleartext (encrypted server-side),
   *  '' clears it. Never send back the masked placeholder. */
  api_key?: string;
  /** Read-only flag indicating a secret is stored. */
  api_key_set?: boolean;
  field_map?: IdentityNotionFieldMap;
}

/** Full response of `identity.config_get`. The Notion api_key is masked. */
export interface IdentityConfig {
  provider: IdentityProviderKind;
  notion: IdentityNotionConfig;
  /** Where the wiki-cache provider reads people records from (display only). */
  wiki_cache?: { people_dir: string };
}

/** Partial update payload for `identity.config_set`. Omit `notion.api_key` to
 *  keep the stored secret; send '' to clear it. */
export interface IdentityConfigUpdate {
  provider?: IdentityProviderKind;
  notion?: IdentityNotionConfig;
}

/** Canonical person record returned by `identity.resolve`. */
export interface ResolvedPerson {
  person_id: string;
  display_name: string;
  roles: string[];
  project_ids: string[];
  emails: string[];
  channel_handles: Record<string, string>;
  source: string;
  fetched_at: string;
}

/** Response of `identity.resolve`. A miss is `found: false` (not an error). */
export interface IdentityResolveResult {
  found: boolean;
  provider: string;
  channel: string;
  is_project_member?: boolean;
  person?: ResolvedPerson;
}

// ── CAP: per-agent [capabilities] ───────────────────────────────

export type ComputerUseMode = 'container' | 'native' | 'auto';

export interface ComputerUseConfig {
  allowed_apps?: string[];
  blocked_actions?: string[];
  max_session_minutes?: number;
  max_actions?: number;
  display_width?: number;
  display_height?: number;
  auto_confirm_trusted?: boolean;
}

/** One clause of a `ToolPolicy` rule — an argument match tested with `op`. */
export type ToolPolicyOp = 'equals' | 'contains' | 'starts_with';

export interface ToolPolicyWhen {
  /** Argument name to test on the tool call. */
  arg: string;
  op: ToolPolicyOp;
  value: string;
}

export type ToolPolicyEffect = 'allow' | 'forbid' | 'ask';

/** A single Progent-style tool-authorization rule. When `policy` is non-empty
 *  the agent runs in strict-allowlist mode (forbid > ask > allow; no allow
 *  match ⇒ deny). `effect: "ask"` escalates to human approval. `when` clauses
 *  are ANDed; absent/empty `when` matches any call to `tool`. `tool: "*"`
 *  matches every tool. */
export interface ToolPolicyRule {
  tool: string;
  effect: ToolPolicyEffect;
  when?: ToolPolicyWhen[];
}

/** How much the autonomous goal loop may drive an agent on its own
 *  (`agent.toml [capabilities] autonomy_level`, parsed by
 *  `goal_loop::AutonomyLevel`). From most human-driven to most autonomous:
 *  `operator` (the loop never touches this agent) → `collaborator` /
 *  `consultant` (a human must approve before the loop kicks a goal off) →
 *  `approver` (no kickoff gate; the default — pauses and waits when stuck) →
 *  `observer` (fully autonomous; a stuck task is just reported, never
 *  waited on). An unrecognized string fails closed to `approver` server-side. */
export type AutonomyLevel = 'operator' | 'collaborator' | 'consultant' | 'approver' | 'observer';

/** The `capabilities` object accepted by `agents.update`. All fields optional —
 *  the backend only writes fields that are present (partial update). */
export interface AgentCapabilities {
  computer_use?: boolean;
  computer_use_mode?: ComputerUseMode;
  browser_via_bash?: boolean;
  allowed_tools?: string[];
  denied_tools?: string[];
  wiki_visible_to?: string[];
  computer_use_config?: ComputerUseConfig;
  /** OS-level sandbox (macOS Seatbelt / Linux Landlock) for tool execution. */
  native_sandbox?: boolean;
  /** Progent tool-authorization policy. Empty/absent = not enforced. */
  policy?: ToolPolicyRule[];
  /** v1.39 — opt-in OS-native features (filesystem watchers via [os_watch]). */
  os_native?: boolean;
  /** WP3.3 — opt-in recording-to-skill capture (browser/desktop recording). */
  recording?: boolean;
  /** WP-10A — opt-in hand-off of the operator's own SSH/GPG identity (push +
   *  commit-signing credentials) to this agent's spawned CLI subprocess.
   *  Danger-zone capability: default false. */
  git_credentials?: boolean;
  /** O-4 — opt-in system-operator designation: master switch for the `os_*`
   *  system-operation tool face (device/system status, backup, power,
   *  update, factory reset, doctor). A materially higher trust tier than
   *  `os_native` (own-machine automation only) — this marks the agent as
   *  allowed to operate the whole host on the operator's behalf. Danger-zone
   *  capability: default false, denied at the dispatch gate even for an
   *  Admin-scoped agent until explicitly enabled here. */
  system_operator?: boolean;
  /** CD-1 — opt-in human-machine co-drive: master switch for the
   *  `codrive_run` MCP tool (GUI mouse/keyboard injection into a shared
   *  desktop via the duduclaw-comp compositor, human-supervised throughout).
   *  Danger-zone capability: default false, denied at the dispatch gate even
   *  for an Admin-scoped agent until explicitly enabled here. */
  codrive?: boolean;
  /** How much the autonomous goal loop may drive this agent on its own. */
  autonomy_level?: AutonomyLevel;
}

/** v1.39 — top-level `[os_watch]` table (gated by `capabilities.os_native`).
 *  Paths are format-checked only; existence is verified at watcher start. */
export interface OsWatchConfig {
  paths?: string[];
  ignore?: string[];
  debounce_ms?: number;
  max_events_per_min?: number;
}

// ── OS: dashboard "OS" page (P4-3) — OS-native fleet status + settings ──

/** Machine-readable code the gateway returns when a write would push the
 *  number of OS-native agents past the edition quota. Stable string — the
 *  dashboard keys UI copy off it (never displays it directly). */
export const OS_NATIVE_QUOTA_ERROR_CODE = 'os_native_quota_exceeded';

export interface OsQuota {
  /** null ⇒ unlimited (Enterprise). */
  limit: number | null;
  used: number;
}

export interface OsAgentWatch {
  paths: string[];
  events: number;
  dropped: number;
}

export interface OsAgentFrontmost {
  /** 0 ⇒ polling not configured. */
  poll_secs: number;
  running: boolean;
}

export interface OsAgentProactive {
  enabled: boolean;
  base_threshold: number;
  max_per_hour: number;
}

/** One row of `os.status` — a fleet member's OS-native snapshot. */
export interface OsAgentStatus {
  /** Registry name — also what `os.settings.update` expects as `agent_id`. */
  agent_id: string;
  os_native: boolean;
  watch: OsAgentWatch;
  frontmost: OsAgentFrontmost;
  footprint: boolean;
  proactive: OsAgentProactive;
  /** PBD-induced (P4-1) autopilot rules that target this agent (best-effort). */
  induced_rules_count: number;
}

export interface OsStatusResult {
  edition: 'personal' | 'enterprise';
  quota: OsQuota;
  agents: OsAgentStatus[];
}

/** Partial update payload for `os.settings.update`. Mirrors the flat OS-page
 *  shape the gateway remaps onto `agents.update` internally. */
export interface OsSettingsUpdateParams {
  agent_id: string;
  os_native?: boolean;
  footprint?: boolean;
  /** 0 = disabled, 1-3600. */
  frontmost_poll_secs?: number;
  proactive?: {
    enabled?: boolean;
    /** 1-5. */
    base_threshold?: number;
    /** 0-1000. */
    max_per_hour?: number;
  };
  os_watch?: OsWatchConfig;
}

export interface OsSettingsUpdateResult {
  success: true;
  agent_id: string;
  hot_reloaded: boolean;
  os_watch_hot_reloaded: boolean;
  message: string;
}

export type OsGateDecision = 'allow' | 'suppress';

/** One `proactive_gate.jsonl` row (raw shape — the gate's own log format). */
export interface OsGateRow {
  ts: string;
  agent: string;
  event: string;
  score: number;
  threshold: number;
  interruptibility: number;
  decision: OsGateDecision;
  reason: string;
  latency_ms?: number;
  outcome?: string | null;
}

/** Four-quadrant (plus non-response / unknown) outcome tally for the
 *  proactivity confusion matrix. */
export interface OsGateQuadrants {
  correct_detection: number;
  false_alarm: number;
  missed_need: number;
  non_response: number;
  correct_silence: number;
  unknown: number;
}

export interface OsGateRecentResult {
  recent: OsGateRow[];
  quadrants: OsGateQuadrants;
}

export type OsEventKind = 'os_file' | 'os_frontmost' | string;

export interface OsEventRow {
  id: number;
  event: OsEventKind;
  ts: string;
  source: string | null;
  payload: Record<string, unknown>;
}

export interface OsEventsRecentResult {
  events: OsEventRow[];
}

/** `os.events.entry` live-push payload (P4-3+) — same shape as one
 *  `OsEventRow`, minus `id`: a live push races the async `events.db`
 *  persistence bridge, so there is no DB id to report yet. The caller
 *  synthesizes its own list key. */
export type OsEventPush = Omit<OsEventRow, 'id'>;

export type OsDoctorCheckId = 'notification' | 'frontmost' | 'calendar' | 'spotlight';
export type OsDoctorStatus = 'ok' | 'warn' | 'fail' | 'skip';

export interface OsDoctorCheck {
  id: OsDoctorCheckId;
  status: OsDoctorStatus;
  detail: string;
}

export interface OsDoctorRunResult {
  checks: OsDoctorCheck[];
}

// ── CON: per-agent CONTRACT.toml ────────────────────────────────

export interface ContractConfig {
  must_not: string[];
  must_always: string[];
  max_tool_calls_per_turn: number;
}

// ── RED: global [redaction] ─────────────────────────────────────

export type RedactionSourceMode = 'on' | 'off' | 'selective' | 'inherit';

/** One source's setting: mode + optional per-source field (category) filter.
 *  Both lists empty = redact every category the active profiles cover. */
export interface RedactionSourceSetting {
  mode: RedactionSourceMode;
  /** When non-empty, ONLY these categories are redacted for this source. */
  only_categories: string[];
  /** Categories never redacted for this source (wins over only_categories). */
  exclude_categories: string[];
}

export interface RedactionSources {
  user_input: RedactionSourceSetting;
  tool_results: RedactionSourceSetting;
  system_prompt: RedactionSourceSetting;
  sub_agent: RedactionSourceSetting;
  cron_context: RedactionSourceSetting;
}

/** One profile in the field-picker catalogue (built-in or custom). */
export interface RedactionProfileInfo {
  name: string;
  description: string;
  builtin: boolean;
  rule_count: number;
  /** PII categories (fields) this profile detects, e.g. "TW_ID", "EMAIL". */
  categories: string[];
}

export type RedactionRestoreArgs = 'restore' | 'passthrough' | 'deny';

export interface RedactionEgressRule {
  restore_args: RedactionRestoreArgs;
  audit_reveal: boolean;
}

export interface RedactionConfig {
  enabled: boolean;
  vault_ttl_hours: number;
  purge_after_expire_days: number;
  profiles: string[];
  sources: RedactionSources;
  tool_egress: Record<string, RedactionEgressRule>;
  /** Catalogue of selectable profiles + the fields each covers. */
  available_profiles: RedactionProfileInfo[];
}

/** Partial update payload for `redaction.update`. A `tool_egress` value of
 *  `null` removes that tool's rule. */
export interface RedactionUpdate {
  enabled?: boolean;
  vault_ttl_hours?: number;
  purge_after_expire_days?: number;
  profiles?: string[];
  sources?: Partial<RedactionSources>;
  tool_egress?: Record<string, RedactionEgressRule | null>;
}

/** Vault counters from `redaction.stats`. `by_category` is a list of
 *  `[category, count]` tuples (e.g. `["EMAIL", 12]`). */
export interface RedactionVaultStats {
  total: number;
  active: number;
  expired: number;
  by_category: Array<[string, number]>;
}

/** Response of `redaction.stats`. When the redaction manager is off, the
 *  gateway returns a zeroed shape with `enabled: false`. */
export interface RedactionStats {
  vault: RedactionVaultStats;
  rule_count: number;
  config_enabled: boolean;
  vault_ttl_hours: number;
  /** Present only in the manager-absent fallback shape. */
  enabled?: boolean;
}

/** One audit line from `redaction.recent_audit`. The `event` tag discriminates
 *  the record (redact / restore_ok / restore_denied / …); fields vary per event
 *  so this is intentionally an open record. */
export interface RedactionAuditEntry {
  event: string;
  ts?: string;
  agent_id?: string;
  category?: string;
  token?: string;
  caller?: string;
  target?: string;
  tool?: string;
  reason?: string;
  [key: string]: unknown;
}

/** Response of `redaction.policy_status`. */
export interface RedactionPolicyStatus {
  config_enabled: boolean;
  vault_ttl_hours: number;
  purge_after_expire_days: number;
  rule_count: number;
  override_active: boolean;
}

/** Response of `redaction.override_status`. `record` carries the operator +
 *  reason when a force-reveal override is active. */
export interface RedactionOverrideStatus {
  active: boolean;
  banner: string | null;
  record: {
    started_at: string;
    operator: string;
    channels: string[];
    reason: string;
  } | null;
}

// ── EVO: evolution-event audit query (`audit.evolution_query`) ──────

/** One evolution event from `audit.evolution_query`. `event_type` and
 *  `outcome` are stringified enum labels; `metadata` is an open object. */
export interface EvolutionEvent {
  timestamp: string;
  event_type: string;
  agent_id: string | null;
  skill_id: string | null;
  generation: number | null;
  outcome: string;
  trigger_signal: string | null;
  metadata: Record<string, unknown>;
}

/** Filters accepted by `audit.evolution_query`. All optional. */
export interface EvolutionQueryFilter {
  agent_id?: string;
  event_type?: string;
  outcome?: string;
  skill_id?: string;
  since?: string;
  until?: string;
  limit?: number;
  offset?: number;
}

/** Response of `audit.evolution_query`. */
export interface EvolutionQueryResult {
  events: EvolutionEvent[];
  total: number;
  limit: number;
  offset: number;
}

// ── TOOLS: platform tool catalog (`tools.catalog`) ─────────────────

/** One entry in the global `tools.catalog` — a platform-wide capability
 *  available to agents. Not per-agent. */
export interface ToolCatalogEntry {
  name: string;
  description: string;
}

/** One entry in the built-in tool catalog (`tools.builtin_catalog`) — a
 *  concrete tool an agent can be granted/denied via `[capabilities]
 *  allowed_tools` / `denied_tools`. Sourced from `duduclaw_core::tool_catalog`,
 *  the single source of truth that mirrors `mcp_auth::tool_requires_scope`. */
export interface BuiltinToolEntry {
  /** Bare tool name, e.g. `office_script` or `Bash`. */
  name: string;
  /** The exact string to add to the allow/deny list. MCP tools are
   *  `mcp__duduclaw__<name>`; native Claude tools use the bare name. */
  qualified: string;
  description: string;
  /** MCP scope Display form (e.g. `skill:execute`), or `''` for native tools. */
  scope: string;
  /** UI grouping category (`channel` / `memory` / `wiki` / `office` / ...). */
  category: string;
  /** `mcp` for DuDuClaw MCP tools, `claude` for native Claude Code tools. */
  kind: string;
}

// ── SKS: global [skill_synthesis] auto-run (W19-P1) ─────────────

/** Skill-synthesis auto-run scheduler config from `skill_synthesis.get`. */
export interface SkillSynthesisConfig {
  /** Master switch — when false the scheduler never runs the pipeline. */
  auto_run: boolean;
  /** When true, score+log only (no Skill Bank writes). */
  dry_run: boolean;
  /** Interval between runs, in hours (>= 1). */
  interval_hours: number;
  /** Days of EvolutionEvents JSONL scanned per run (1-30). */
  lookback_days: number;
  /** Owner of synthesized skills; empty string = fall back to default_agent. */
  target_agent: string;
}

/** Partial update payload for `skill_synthesis.update`. */
export type SkillSynthesisUpdate = Partial<SkillSynthesisConfig>;

// ── MK: MCP API keys ────────────────────────────────────────────

/** A masked MCP key entry from `mcp_keys.list`. The full cleartext key is
 *  NEVER returned here — only on the one-time `mcp_keys.create` response. */
export interface McpKeyEntry {
  masked: string;
  client_id: string;
  is_external: boolean;
  created_at: string;
  scopes: string[];
  rotate_recommended: boolean;
}

export interface McpKeyCreateResult {
  success: boolean;
  /** Cleartext key — shown exactly once, never recoverable afterward. */
  key: string;
  masked: string;
  client_id: string;
  is_external: boolean;
  created_at: string;
  scopes: string[];
  message: string;
}

export type McpScope =
  | 'memory:read'
  | 'memory:write'
  | 'wiki:read'
  | 'wiki:write'
  | 'messaging:send'
  | 'identity:read'
  | 'odoo:read'
  | 'odoo:write'
  | 'odoo:execute'
  | 'google:read'
  | 'google:write'
  | 'notion:read'
  | 'notion:write'
  | 'github:read'
  | 'github:write'
  | 'fork:execute'
  | 'os:native'
  | 'skill:execute'
  | 'recording'
  | 'mail:read'
  | 'mail:send'
  | 'admin';

/** All known MCP scopes — mirrors the shared canonical list
 *  (`duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS`, read by both the gateway's
 *  `KNOWN_MCP_SCOPES` validator and `duduclaw-cli::mcp_auth::Scope`). Was a
 *  10-entry list that had drifted from the real 22 scopes (2026-08 audit) —
 *  dashboard operators could not grant 12 of them without hand-editing
 *  config.toml. */
export const MCP_SCOPES: ReadonlyArray<McpScope> = [
  'memory:read',
  'memory:write',
  'wiki:read',
  'wiki:write',
  'messaging:send',
  'identity:read',
  'odoo:read',
  'odoo:write',
  'odoo:execute',
  'google:read',
  'google:write',
  'notion:read',
  'notion:write',
  'github:read',
  'github:write',
  'fork:execute',
  'os:native',
  'skill:execute',
  'recording',
  'mail:read',
  'mail:send',
  'admin',
];

// ── COST: cache-efficiency telemetry (`cost.*`) ─────────────────

/** 200K-token price-cliff analysis from `cost.summary`. `warning` trips when
 *  requests are approaching / crossing the input-token threshold. */
export interface CostPriceCliff {
  threshold_input_tokens: number;
  requests_near_cliff: number;
  max_input_tokens: number;
  warning: boolean;
}

/** Response of `cost.summary`. `available:false` = telemetry not initialized;
 *  all numeric fields are then absent. Costs are in millicents (1 cent =
 *  1000 millicents). */
export interface CostSummary {
  available: boolean;
  period?: string;
  total_requests?: number;
  total_input_tokens?: number;
  total_cache_read_tokens?: number;
  total_cache_creation_tokens?: number;
  total_output_tokens?: number;
  /** 0.0–1.0 mean cache efficiency. */
  avg_cache_efficiency?: number;
  /** 0.0–1.0 overall cache hit rate. */
  cache_hit_rate?: number;
  total_cost_millicents?: number;
  total_cache_savings_millicents?: number;
  price_cliff?: CostPriceCliff;
}

export type CacheHealth = 'healthy' | 'normal' | 'degraded';

export interface CostAgentRow {
  agent_id: string;
  cache_health: CacheHealth;
  total_requests: number;
  total_input_tokens?: number;
  total_cache_read_tokens?: number;
  total_cache_creation_tokens?: number;
  total_output_tokens?: number;
  avg_cache_efficiency: number;
  total_cost_millicents: number;
  total_cache_savings_millicents: number;
}

export interface CostAgentsResult {
  available: boolean;
  agents: CostAgentRow[];
}

export interface CostRecentRow {
  agent_id: string;
  request_type: string;
  model: string;
  input_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens?: number;
  output_tokens?: number;
  /** 0.0–1.0. */
  cache_efficiency: number;
  cost_millicents: number;
  cache_savings_millicents: number;
  created_at: string;
}

export interface CostRecentResult {
  available: boolean;
  records: CostRecentRow[];
}

// ── NOTIFY: notification governance action-rate telemetry (`notify.stats`, W2-4/W2-8) ──

/** One notification type's scorecard over the queried window (`notify_stats.rs`
 *  `NotifyTypeStat`). `notify_type` is a free-form `<family>.<what>` bucket
 *  (e.g. `decision.approval`, `evolution.stagnation`) — there is no fixed
 *  enum, new subsystems add new buckets over time. */
export interface NotifyTypeStat {
  type: string;
  /** Distinct pushes in the window. */
  pushed: number;
  /** Pushes that carried something to press — 0 for a plain FYI line, which
   *  by definition has nothing to act on. */
  actionable: number;
  /** Actionable pushes a person actually settled. */
  acted: number;
  /** `acted / actionable`, 0.0–1.0 (0 when nothing was actionable). */
  action_rate: number;
  /** The SRE 50% rule (P4-5): enough actionable samples (`min_sample`) AND
   *  fewer than half acted on. Never `true` for an all-FYI type — see
   *  `actionable`. */
  broken: boolean;
}

export interface NotifyStatsResponse {
  days: number;
  /** The action-rate threshold below which a type is flagged `broken` (0.5). */
  broken_threshold: number;
  /** Minimum actionable-push sample size before `broken` can ever be `true`. */
  min_sample: number;
  types: NotifyTypeStat[];
}

// ── MEM: temporal history / supersession chain (`memory.history/at`) ──

/** One version in a fact's supersession chain (`memory.history`). */
export interface MemoryChainEntry {
  id: string;
  content: string;
  valid_from: string | null;
  valid_until: string | null;
  superseded_by: string | null;
  supersedes: string | null;
  confidence: number | null;
  is_current: boolean;
}

/** Response of `memory.history`. An empty `chain` = no recorded history. */
export interface MemoryHistoryResult {
  subject: string;
  predicate: string;
  current_id: string | null;
  chain: MemoryChainEntry[];
}

/** A point-in-time record from `memory.at`. A miss is `found:false`. */
export interface MemoryAtRecord {
  id: string;
  content: string;
  valid_from: string | null;
  valid_until: string | null;
  [key: string]: unknown;
}

export interface MemoryAtResult {
  found: boolean;
  record?: MemoryAtRecord;
}

/** Selector for `memory.history` — either a fact key (subject+predicate) or a
 *  specific memory id. */
export interface MemoryHistoryQuery {
  subject?: string;
  predicate?: string;
  memory_id?: string;
}

// ── D6: SPO knowledge-graph curation (`memory.graph` / `memory.invalidate_origin`) ──

/** One entity node in the exported SPO graph. `degree` = incident valid edges. */
export interface MemoryGraphNode {
  entity: string;
  degree: number;
}

/** One labelled edge (SPO triple) with provenance for the curation viewer. */
export interface MemoryGraphEdge {
  subject: string;
  predicate: string | null;
  object: string | null;
  memory_id: string;
  /** Source-confidence tier (0–1) driving the node/edge colour. */
  origin_trust: number;
  /** Held for human review (excluded from retrieval until released). */
  quarantined: boolean;
}

/** Response of `memory.graph`. `truncated` = the newest-first cut kicked in. */
export interface MemoryGraphResult {
  nodes: MemoryGraphNode[];
  edges: MemoryGraphEdge[];
  truncated: boolean;
}

// ── ODO: per-agent Odoo credential override (`odoo.agent_config_*`) ──

/** Response of `odoo.agent_config_get`. `configured:false` = no override, the
 *  agent inherits the global config. `api_key`/`password` are never returned in
 *  cleartext — only the `*_set` booleans plus a masked placeholder. */
export interface OdooAgentConfig {
  agent_id: string;
  configured: boolean;
  profile?: string;
  url?: string;
  db?: string;
  username?: string;
  allowed_models: string[];
  /** Models opted out of the built-in security block list. */
  unblock_models?: string[];
  allowed_actions: string[];
  company_ids: number[];
  api_key_set: boolean;
  /** Masked placeholder ("***set***") when a key is stored, else absent. */
  api_key?: string;
  password_set: boolean;
}

/** Partial update payload for `odoo.agent_config_set`. `api_key`/`password`
 *  are write-only: send a new value to set, `''` to clear, omit to keep.
 *  Sending back the masked placeholder is rejected server-side (no-op). */
export interface OdooAgentConfigSet {
  agent_id: string;
  url?: string;
  db?: string;
  user?: string;
  api_key?: string;
  password?: string;
  profile?: string;
  allowed_models?: string[];
  unblock_models?: string[];
  allowed_actions?: string[];
  company_ids?: number[];
}

export interface OdooAgentConfigSetResult {
  success: boolean;
  changes: string[];
  hot_reloaded: boolean;
}

// ── KS: KILLSWITCH.toml ─────────────────────────────────────────

export interface KillswitchTriggers {
  max_replies_per_minute: number;
  max_consecutive_errors: number;
  error_rate_threshold: number;
  cost_limit_usd: number;
}

export interface KillswitchCircuitBreaker {
  frequency_window_secs: number;
  frequency_max_replies: number;
  similarity_threshold: number;
  token_explosion_multiplier: number;
  cooldown_secs: number;
  half_open_allow_count: number;
}

export interface KillswitchFailsafe {
  l1_auto_recover_secs: number;
  l2_auto_recover_secs: number;
  l3_auto_recover_secs: number;
  default_restricted_reply: string;
  default_halted_reply: string;
}

export interface KillswitchSafetyWords {
  stop: string[];
  stop_all: string[];
  resume: string[];
  status: string[];
}

export interface KillswitchDefensivePrompt {
  enabled: boolean;
  languages: string[];
}

export interface KillswitchAudit {
  enabled: boolean;
  path: string;
}

export interface KillswitchConfig {
  triggers: KillswitchTriggers;
  circuit_breaker: KillswitchCircuitBreaker;
  failsafe: KillswitchFailsafe;
  safety_words: KillswitchSafetyWords;
  defensive_prompt: KillswitchDefensivePrompt;
  audit: KillswitchAudit;
}

export interface KillswitchUpdate {
  triggers?: Partial<KillswitchTriggers>;
  circuit_breaker?: Partial<KillswitchCircuitBreaker>;
  failsafe?: Partial<KillswitchFailsafe>;
  safety_words?: Partial<KillswitchSafetyWords>;
  defensive_prompt?: Partial<KillswitchDefensivePrompt>;
  audit?: Partial<KillswitchAudit>;
}

// ── GOV: governance policies (policies/*.yaml) ──────────────────

export type GovPolicyType = 'rate' | 'permission' | 'quota' | 'lifecycle';

/** Valid `rate` policy resources — mirrors gateway `GOV_RATE_RESOURCES`. */
export const GOV_RATE_RESOURCES = ['mcp_calls', 'memory_writes', 'wiki_writes', 'message_sends'] as const;
export type GovRateResource = (typeof GOV_RATE_RESOURCES)[number];

/** Valid `rate` violation actions — mirrors gateway `GOV_ACTIONS`. */
export const GOV_ACTIONS = ['reject', 'warn', 'throttle'] as const;
export type GovAction = (typeof GOV_ACTIONS)[number];

export const GOV_POLICY_TYPES = ['rate', 'permission', 'quota', 'lifecycle'] as const;

/** A governance policy. The shape is a discriminated union on `policy_type`,
 *  but the backend stores/returns a flat object — we keep it flat with all
 *  per-type fields optional. `scope` is read-only (added by `governance.list`):
 *  "global" or an agent id. `agent_id` is "*" (global) or a valid agent id. */
export interface GovPolicy {
  policy_type: GovPolicyType;
  policy_id: string;
  agent_id: string;
  scope?: string;
  // rate
  resource?: GovRateResource;
  limit?: number;
  window_seconds?: number;
  action_on_violation?: GovAction;
  // permission
  allowed_scopes?: string[];
  denied_scopes?: string[];
  requires_approval?: string[];
  // quota
  daily_token_budget?: number;
  max_concurrent_tasks?: number;
  max_memory_entries?: number;
  reset_cron?: string;
  // lifecycle
  max_idle_hours?: number;
  health_check_interval_seconds?: number;
  auto_suspend_on_violation_count?: number;
}

// ── SCP: wiki namespace policy (.scope.toml) ────────────────────

export type WikiScopeMode = 'agent_writable' | 'read_only' | 'operator_only';

export interface WikiScopeNamespace {
  namespace: string;
  mode: WikiScopeMode;
  /** Required (non-empty) when mode === 'read_only'. */
  synced_from: string | null;
}

// ── APR: HITL approval center (WP14-T14.7) ─────────────────────

/** Known approval kinds. Unknown kinds are surfaced verbatim as a fallback. */
export type ApprovalKind =
  | 'browser_action'
  | 'tool_call'
  | 'skill_activation'
  | 'strategic_plan'
  | 'agent_hire'
  | 'wiki_ingest'
  | (string & {});

/** D1/D2: the ActionGuard judge's forward-simulation narrative — "what will
 *  the world look like after this call runs", predicted before the decision
 *  is made. Only present for approval kinds that ran that judge (the
 *  overwhelming majority never do); absent/`null` means "no prediction was
 *  made", not "nothing will happen". */
export interface ApprovalSimulation {
  world_state_change: string;
  risk_points: string[];
}

export interface ApprovalItem {
  id: string;
  agent_id: string;
  kind: ApprovalKind;
  summary: string;
  /** Opaque request payload — shape varies by kind. */
  payload: unknown;
  created_at: string;
  ttl_seconds: number;
  /** Unix epoch (seconds) this approval auto-denies at (`created_at +
   *  ttl_seconds`, computed server-side). `null` on an unparseable
   *  `created_at` — treat as "no countdown available", not "never expires". */
  expires_at?: number | null;
  /** Absent on older/other approval kinds — render nothing, not a placeholder. */
  simulation?: ApprovalSimulation | null;
  // ── W2-3 reverse handoff (E8) ─────────────────────────────
  /** The channel this approval's decision card was pushed to, when known. */
  channel?: string | null;
  /** Server-resolved "open in `channel`" URL, or `null` when nothing could
   *  be constructed for this platform (never a raw chat/message id — see
   *  `channel_link.rs`). Render the button ONLY when this is a non-empty
   *  string. */
  channel_link?: string | null;
}

// ── BUD: budget incident console (WP14-T14.6) ──────────────────

export interface BudgetIncident {
  ts: string;
  agent_id: string;
  event: string;
  scope: string;
  spent_cents: number;
  cap_cents: number;
}

export interface BudgetByAgent {
  agent_id: string;
  open_events: number;
}

// ── LB: skill leaderboard (WP10-T10.1) ─────────────────────────

export interface SkillLeaderboardEntry {
  skill: string;
  display_name: string;
  estimated_minutes_saved: number;
  scope: string;
  owner: string;
}

// ── MIG: painless migration wizard (migrate.scan / migrate.apply) ──────────

/** Source platforms the migration wizard can import from. */
export type MigratePlatform = 'openclaw' | 'hermes' | 'paperclip';

/** Per-item outcome of a migration scan/apply. Reported honestly — SKIPPED and
 *  CONFLICT carry a reason and are never smoothed over in the UI. */
export type MigrateItemStatus = 'imported' | 'partial' | 'skipped' | 'conflict';

/** Overall verdict aggregating every item's status. */
export type MigrateVerdict = 'COMPLETE' | 'DEGRADED' | 'PARTIAL';

export interface MigrateItem {
  /** Item category, e.g. `agent`, `channel_token`, `skill`, `cron`, `model`. */
  kind: string;
  name: string;
  status: MigrateItemStatus;
  /** Why the item was skipped / partially imported / in conflict. Null when clean. */
  reason: string | null;
}

export interface MigrateSummary {
  imported: number;
  partial: number;
  skipped: number;
  conflict: number;
}

/** Result shape shared by `migrate.scan` (dry_run:true) and `migrate.apply`
 *  (dry_run:false, report_path populated). */
export interface MigrateResult {
  platform: MigratePlatform;
  source: string;
  dry_run: boolean;
  items: MigrateItem[];
  summary: MigrateSummary;
  verdict: MigrateVerdict;
  notes: string[];
  /** Absolute path of the written report — only present after a real apply. */
  report_path: string | null;
}

// ── Branding / white-label (design-distributor-white-label §3.5) ────────────

/** The upstream software vendor block — authored by the backend const, never
 *  writable from the dashboard. Always present in `branding.get` / `about.get`. */
export interface BrandingVendor {
  name_zh: string;
  name_en: string;
  url: string;
}

/** Distributor-authored branding. All fields optional — a null/absent field
 *  means "use the DuDuClaw default". `updated_at` is server-set (read-only). */
export interface BrandingConfig {
  product_name?: string | null;
  subtitle?: string | null;
  /** Inline `data:image/{png,jpeg,webp};base64,…` logo, or null for the default. */
  logo_data_uri?: string | null;
  company_name?: string | null;
  website?: string | null;
  support_email?: string | null;
  description?: string | null;
  /** Sanitized HTML block for the About page (server-sanitized on read). */
  about_html?: string | null;
  /** Brand accent color as `#rrggbb`, or null for the default amber. */
  accent_color?: string | null;
  updated_at?: string | null;
}

/** Where the active branding resolved from (design §10.1 resolution order). */
export type BrandingSource = 'local' | 'bundle' | 'default';

/** Server-provided defaults used when a field is unset. */
export interface BrandingDefaults {
  product_name: string;
  subtitle_key: string;
}

/** Response of `branding.get`. */
export interface BrandingGetResponse {
  branding: BrandingConfig;
  vendor: BrandingVendor;
  defaults: BrandingDefaults;
  white_label_active: boolean;
  /** Which layer the active branding resolved from (local / bundle / default). */
  source: BrandingSource;
  /** WP8: branding field names this instance may edit (serde keys, 1:1 with the
   *  `branding.set` payload). Empty ⇒ no field is editable (fail-closed). Fields
   *  absent from this list are provider-managed and must be masked in the form. */
  editable_fields: string[];
}

/** Writable subset accepted by `branding.set` (mirrors the backend whitelist). */
export interface BrandingSetInput {
  product_name?: string;
  subtitle?: string;
  logo_data_uri?: string;
  company_name?: string;
  website?: string;
  support_email?: string;
  description?: string;
  about_html?: string;
  accent_color?: string;
}

/** Response of `about.get` — distributor branding + fixed vendor + version/tier. */
export interface AboutResponse {
  vendor: BrandingVendor;
  branding: BrandingConfig;
  version: string;
  tier: string;
  white_label_active: boolean;
  /** Which layer the About branding resolved from. */
  source: BrandingSource;
}

/** A signed branding bundle (design §10.1) — dropped into a customer's
 *  `~/.duduclaw/branding.bundle.json` to auto-apply the brand with no license. */
export interface BrandingBundle {
  schema: number;
  distributor_id: string;
  subscription_id: string;
  branding: BrandingConfig;
  issued_at: string;
  public_key_id: string;
  signature: string;
}

// ── Distributor management (owner-only, design-distributor-white-label §3) ──

/** A distributor account (owner bookkeeping). */
export interface DistributorProfile {
  id: string;
  name: string;
  contact: string;
  note: string;
  status: string;
  created_at: string;
  updated_at: string;
  /** Licenses issued to this distributor — populated by `distributor.list`. */
  licenses?: IssuedLicense[];
}

/** A license issued to a distributor. `license_blob` is only returned on issue. */
export interface IssuedLicense {
  id: string;
  distributor_id: string;
  subscription_id: string;
  customer_id: string;
  tier: string;
  machine_fingerprint: string;
  issued_at: string;
  expires_at: string;
  status: 'active' | 'revoked';
  revoked_at?: string | null;
  license_blob?: string;
  /** RFC3339 of the last successful control-plane refresh (P2 phone-home).
   *  `null`/absent until the distributor instance phones home at least once. */
  last_refresh_at?: string | null;
}

/** Aggregate counters shown on the distributor console. Rendered defensively
 *  (`?? 0`) since the exact backend field set may extend over time. */
export interface DistributorStats {
  total_distributors?: number;
  active_distributors?: number;
  total_licenses?: number;
  active_licenses?: number;
  revoked_licenses?: number;
}

export interface DistributorInput {
  name: string;
  contact?: string;
  note?: string;
}

export interface DistributorPatch {
  name?: string;
  contact?: string;
  note?: string;
  status?: string;
}

// API namespace
// ── WebChat session history (WP3 — resume past conversations) ───────────────

// ── Google credential paths (service account / Apps Script bridge) ────────

/** One configured-or-not credential section, plus its parse error if broken. */
export interface GoogleCredentialSection {
  configured: boolean;
  /** Present only for the service-account section. */
  key_file?: string;
  /** Present only for the service-account section. */
  subject?: string;
  /** Present only for the Apps Script section. */
  url?: string;
  /** Non-empty when the section exists but cannot be used. */
  error: string;
}

export interface GoogleCredentialsStatus {
  /** `config.toml [integrations] google_workspace` — the master gate. */
  integration_enabled: boolean;
  /** Which path a real tool call would take right now. */
  effective: 'direct' | 'apps_script' | 'none';
  service_account: GoogleCredentialSection;
  apps_script: GoogleCredentialSection;
  /** Scope list to hand a Workspace admin for delegation. */
  required_scopes: string[];
}

export type GoogleCredentialsInput =
  | { mode: 'service_account'; key_file: string; subject: string }
  /** Omit `secret` to keep the stored one while editing the URL. */
  | { mode: 'apps_script'; url: string; secret?: string }
  | { mode: 'none' };

/** One past WebChat session, as returned by `chat.sessions.list`
 *  (newest first, archived excluded). */
export interface ChatSessionSummary {
  session_id: string;
  agent_id: string;
  /** Auto-generated title following the discussion (background titler);
   *  falls back to the first user message. CJK-safe 80-char cap. May be empty. */
  title: string;
  /** RFC3339 timestamp of the last activity. */
  last_active: string;
  turns: number;
  tokens: number;
  /** Session lineage marker — opaque, not rendered by the dashboard. */
  lineage?: unknown;
}

export interface ChatSessionMessage {
  role: string;
  content: string;
  /** RFC3339 timestamp. */
  timestamp: string;
  tokens: number;
}

export interface ChatSessionHistory {
  session_id: string;
  agent_id: string;
  messages: ChatSessionMessage[];
}

// ── Industry template packs (premium roster staging, admin-only) ──────────

export interface TemplateIndustrySummary {
  industry: string;
  label: string;
  pack: string;
  worker_count: number;
}

export interface TemplatesIndustriesResponse {
  /** Premium templates unlocked by the active license. */
  unlocked: boolean;
  /** Template resources shipped with the install but locked by the license. */
  present_but_locked: boolean;
  /** Currently staged industry id, if any. */
  staged: string | null;
  /** The generic CEO role is available even without staging an industry. */
  ceo_available: boolean;
  industries: TemplateIndustrySummary[];
}

export type TemplateRoleKind = 'ceo' | 'front_desk' | 'worker';

export interface TemplateRoleSummary {
  role_id: string;
  kind: TemplateRoleKind;
  kit?: string;
  name: string;
  display_name: string;
  summary: string;
  /** An agent has already been created from this role. */
  created: boolean;
  overlay_count: number;
}

export interface TemplateRosterHuman {
  title: string;
  summary: string;
}

export interface TemplateRosterExcluded {
  kit: string;
  reason: string;
}

export interface TemplateRoster {
  industry: string | null;
  label: string | null;
  roles: TemplateRoleSummary[];
  /** Positions deliberately kept human (not deployed as AI staff). */
  humans: TemplateRosterHuman[];
  /** Kits deliberately excluded from deployment, with the reason. */
  excluded: TemplateRosterExcluded[];
}

export interface TemplateRoleDetail {
  role_id: string;
  kind: TemplateRoleKind;
  name: string;
  display_name: string;
  trigger: string;
  reports_to: string | null;
  summary: string;
  soul_md: string;
  contract_toml: string;
  agent_toml: string;
  has_extras: boolean;
}

export interface TemplateCreateAgentParams {
  role_id: string;
  industry?: string;
  name?: string;
  display_name?: string;
  trigger?: string;
  /** Omit ⇒ keep the template's wiring (workers report to the pack's front desk). */
  reports_to?: string;
  department?: string;
  /** Omit ⇒ template default. Backend validates TOML fields server-side. */
  soul_md?: string;
  contract_toml?: string;
  agent_toml?: string;
}

/** How a custom widget was authored. */
export type CustomWidgetOrigin = 'html' | 'ai';

/** Custom widget list row (html stripped; lazy-load via widgetsCustom.get). */
export interface CustomWidgetSummary {
  id: string;
  title: string;
  description: string;
  origin: CustomWidgetOrigin;
  created_by_user: string;
  shared: boolean;
  html_bytes: number;
  created_at: string;
  updated_at: string;
}

/** One entry of a saved home layout (WP15 personal dashboard). */
export interface DashboardLayoutWidget {
  id: string;
  hidden: boolean;
}
export interface DashboardLayout {
  schema: number;
  widgets: DashboardLayoutWidget[];
}

/** One row of `departments.list` — a department exists when any agent, wiki
 *  sub-tree, or skill sub-tree references it (WP7 derived design). */
export interface DepartmentInfo {
  name: string;
  agent_count: number;
  members: string[];
  wiki_pages: number;
  skills: number;
}

export interface TemplateCreateAgentResult {
  success: boolean;
  warning: string | null;
  agent: { name: string; role: string; role_id: string };
}

export const api = {
  /** WebChat past-conversation browsing + resume (WP3). Goes through the
   *  dashboard RPC (authz enforced server-side — a non-admin caller must pass a
   *  visible `agent_id`; other agents' sessions are never returned). */
  chatSessions: {
    list: (params: { agent_id?: string; limit?: number }) =>
      client.call('chat.sessions.list', params) as Promise<{ sessions: ChatSessionSummary[] }>,
    history: (sessionId: string, limit?: number) =>
      client.call('chat.sessions.history', {
        session_id: sessionId,
        ...(limit != null ? { limit } : {}),
      }) as Promise<ChatSessionHistory>,
  },
  agents: {
    /** WP4 — pass `include_archived: true` to also list archived AI staff
     *  (hidden by default). */
    list: (params?: { include_archived?: boolean }) =>
      client.call('agents.list', params ?? {}) as Promise<{ agents: AgentDetail[] }>,
    status: (agentId: string) =>
      client.call('agents.status', { agent_id: agentId }) as Promise<AgentDetail>,
    create: (params: {
      name: string;
      display_name: string;
      role?: string;
      trigger?: string;
      soul?: string;
      /** Supervisor agent name — must already exist. Omit/empty ⇒ standalone. */
      reports_to?: string;
      /** WP7 department (ASCII alphanumeric + '-'/'_'). Omit/empty ⇒ none. */
      department?: string;
      /** Optional `[runtime]` written at create time (e.g. onboarding picks a
       *  non-Claude backend). Omit ⇒ defaults to Claude. */
      runtime?: AgentRuntime;
      /** Preferred model chosen in the create form (unified id). Omit ⇒ server
       *  falls back to a default for programmatic callers only. */
      model_preferred?: string;
    }) =>
      client.call('agents.create', params) as Promise<{ success: boolean; agent: AgentInfo }>,
    /** Fire-and-forget bus message to one agent. No dashboard surface calls
     *  this since UX plan I-1a: the employee-card 交辦 action now opens the
     *  AssignSheet, which files a trackable, judge-verified goal task instead.
     *  Kept as the thin wrapper for the RPC the gateway still serves. */
    delegate: (agentId: string, prompt: string) =>
      client.call('agents.delegate', { agent_id: agentId, prompt }) as Promise<{
        success: boolean;
        message_id: string;
      }>,
    pause: (agentId: string) =>
      client.call('agents.pause', { agent_id: agentId }) as Promise<{ success: boolean }>,
    resume: (agentId: string) =>
      client.call('agents.resume', { agent_id: agentId }) as Promise<{ success: boolean }>,
    inspect: (agentId: string) =>
      client.call('agents.inspect', { agent_id: agentId }) as Promise<AgentDetail>,
    /** E1 — lightweight avatar-only fetch. Unlike `inspect` this does NOT run a
     *  telemetry aggregate or serialize SOUL/skills/model config; it reads only
     *  the uploaded `avatar.<ext>` bytes. Used by the avatar store for first-paint
     *  images (roster/sidebar/chat) so N staff members don't fire N heavy RPCs. */
    avatar: (agentId: string) =>
      client.call('agents.avatar', { agent_id: agentId }) as Promise<{
        agent_id: string;
        has_avatar: boolean;
        avatar: string | null;
      }>,
    update: (agentId: string, fields: AgentUpdateParams) =>
      client.call('agents.update', { agent_id: agentId, ...fields }) as Promise<{
        success: boolean;
        /** Save-time auto-align: provider the gateway rewrote `[runtime]` to
         *  (model↔provider family mismatch), or null when untouched. */
        runtime_provider_aligned?: string | null;
      }>,
    /** WP4 — soft-delete: the AI staff member is hidden from every list but its
     *  data is retained on disk (not recoverable via the UI). */
    remove: (agentId: string) =>
      client.call('agents.remove', { agent_id: agentId }) as Promise<{
        success: boolean;
        agent_id: string;
        status: 'deleted';
        data_retained: boolean;
      }>,
    /** WP4 — archive (recoverable off-board). Rejected for the main agent. */
    archive: (agentId: string) =>
      client.call('agents.archive', { agent_id: agentId }) as Promise<{
        success: boolean;
        agent_id: string;
        status: 'archived';
      }>,
    /** WP4 — restore an archived AI staff member. */
    unarchive: (agentId: string) =>
      client.call('agents.unarchive', { agent_id: agentId }) as Promise<{
        success: boolean;
        agent_id: string;
        status: string;
      }>,
    /** WP4 — hand off memory / wiki / open tasks to another AI staff member,
     *  then (by default) archive the source. Every sub-move is optional (all
     *  default true). A failure in any sub-move returns `status: "PARTIAL"`
     *  with a populated `errors[]` — never silently swallowed. */
    handoff: (params: AgentHandoffParams) =>
      client.call('agents.handoff', { ...params }) as Promise<AgentHandoffResult>,
    /** WP4 — upload an avatar image (png/jpeg/webp data URI, ≤512 KB). */
    /** Save the wardrobe composition; `outfit: null` clears back to the
     *  seeded default look. Purely cosmetic — never affects behaviour. */
    setOutfit: (agentId: string, outfit: import('./outfit').AgentOutfit | null) =>
      client.call('agents.set_outfit', { agent_id: agentId, outfit }) as Promise<{
        success: boolean;
        agent_id: string;
        outfit: import('./outfit').AgentOutfit | null;
      }>,
    setAvatar: (agentId: string, dataUri: string) =>
      client.call('agents.set_avatar', { agent_id: agentId, data_uri: dataUri }) as Promise<{
        success: boolean;
        agent_id: string;
        has_avatar: boolean;
        bytes: number;
      }>,
    /** WP4 — remove an agent's uploaded avatar (no-op-safe). */
    clearAvatar: (agentId: string) =>
      client.call('agents.clear_avatar', { agent_id: agentId }) as Promise<{
        success: boolean;
        agent_id: string;
        has_avatar: boolean;
      }>,
  },
  /** Industry template packs — stage a premium roster, then create AI staff
   *  from the staged roles one by one (all admin-only, license-gated). */
  templates: {
    industries: () =>
      client.call('templates.industries', {}) as Promise<TemplatesIndustriesResponse>,
    /** Stage an industry pack (prepares templates, creates NO agents). */
    stage: (industry: string) =>
      client.call('templates.stage', { industry }) as Promise<{
        success: boolean;
        roster: TemplateRoster;
      }>,
    /** Omit `industry` ⇒ the already-staged one; unstaged still returns CEO. */
    roster: (industry?: string) =>
      client.call('templates.roster', industry ? { industry } : {}) as Promise<TemplateRoster>,
    role: (roleId: string, industry?: string) =>
      client.call('templates.role', {
        role_id: roleId,
        ...(industry ? { industry } : {}),
      }) as Promise<TemplateRoleDetail>,
    createAgent: (params: TemplateCreateAgentParams) =>
      client.call('templates.create_agent', { ...params }) as Promise<TemplateCreateAgentResult>,
  },
  dashboard: {
    /** Widgets the current user may place on their home (fail-closed server-side). */
    widgetsCatalog: () =>
      client.call('dashboard.widgets.catalog') as Promise<{ widgets: Array<{ id: string; min_role: string }> }>,
    layoutGet: () =>
      client.call('dashboard.layout.get') as Promise<{ layout: DashboardLayout | null }>,
    layoutSet: (widgets: DashboardLayoutWidget[]) =>
      client.call('dashboard.layout.set', { widgets }) as Promise<{ success: boolean; layout: DashboardLayout }>,
    /** Read-only view of a SUBORDINATE's dashboard (manager+; strict-rank
     *  gate server-side). There is deliberately no set-for-others RPC. */
    layoutView: (userId: string) =>
      client.call('dashboard.layout.view', { user_id: userId }) as Promise<{
        user: { id: string; display_name: string; role: string };
        widgets: Array<{ id: string; min_role: string }>;
        layout: DashboardLayout | null;
        bound_agents: string[];
        /** Custom widgets on the target's layout, with html — the view-as
         *  grant covers rendering them read-only (they may be private). */
        custom_widgets: Array<{ id: string; title: string; html: string }>;
        read_only: true;
      }>,
  },
  widgetsCustom: {
    /** Widgets visible to me: my own + instance-shared. List is html-free.
     *  `max_per_user` is the operator-configured per-user cap (0 = unlimited). */
    list: () =>
      client.call('widgets.custom.list') as Promise<{
        widgets: CustomWidgetSummary[];
        max_per_user: number;
      }>,
    /** Full widget incl. html — lazy-loaded at render/edit time. */
    get: (id: string) =>
      client.call('widgets.custom.get', { id }) as Promise<CustomWidgetSummary & { html: string }>,
    create: (params: { title: string; description?: string; html: string; origin: CustomWidgetOrigin }) =>
      client.call('widgets.custom.create', { ...params }) as Promise<{ success: boolean; id: string }>,
    update: (id: string, params: { title?: string; description?: string; html?: string }) =>
      client.call('widgets.custom.update', { id, ...params }) as Promise<{ success: boolean }>,
    remove: (id: string) =>
      client.call('widgets.custom.remove', { id }) as Promise<{ success: boolean }>,
    share: (id: string, shared: boolean) =>
      client.call('widgets.custom.share', { id, shared }) as Promise<{ success: boolean; shared: boolean }>,
    /** Guided NL generation (P2). Returns draft html only — nothing is stored
     *  until the user previews and explicitly saves via create(). */
    generate: (params: {
      prompt: string;
      style?: string;
      data_sources?: string[];
      prior_html?: string;
      feedback?: string;
    }) =>
      client.call('widgets.custom.generate', { ...params }) as Promise<{ html: string }>,
  },
  departments: {
    list: () =>
      client.call('departments.list') as Promise<{ departments: DepartmentInfo[] }>,
    create: (name: string) =>
      client.call('departments.create', { name }) as Promise<{ success: boolean; name: string }>,
    remove: (name: string, force?: boolean) =>
      client.call('departments.remove', { name, ...(force ? { force } : {}) }) as Promise<{ success: boolean }>,
  },
  runtime: {
    /** Detect installed AI runtime CLIs + Claude OAuth — drives the onboarding
     *  "choose your AI backend" picker. Presence booleans only, no secrets. */
    detect: () =>
      client.call('runtime.detect') as Promise<RuntimeDetect>,
    /** Install a missing CLI on the gateway host (admin). `provider` is the
     *  ONLY parameter — the gateway maps it to a hard-coded command. Returns
     *  as soon as the child is spawned; progress arrives as
     *  `runtime.install.output` / `runtime.install.status` events. */
    install: (provider: InstallableProvider) =>
      client.call('runtime.install', { provider }) as Promise<RuntimeInstallStart>,
  },
  // Expert packs (專家包) — all admin-only, fail-closed server-side.
  experts: {
    list: () =>
      client.call('experts.list') as Promise<{ packs: ExpertPack[] }>,
    /** Install from a server-local path (use `POST /api/experts/upload` to
     *  stage a .zip first — see ExpertsPage). Long-running: security scans run
     *  inside the install pipeline. */
    install: (path: string, attachUnder?: string) =>
      client.call(
        'experts.install',
        { path, ...(attachUnder ? { attach_under: attachUnder } : {}) },
        false,
        320000,
      ) as Promise<{
        success: boolean;
        output: string;
      }>,
    remove: (slug: string) =>
      client.call('experts.remove', { slug }) as Promise<{
        success: boolean;
        items: Array<{ status: string; kind: string; name: string; detail: string }>;
      }>,
    /** Apply the approval-center decision for a pack's hooks (approve → enable;
     *  deny/expire → stay disabled). */
    hooksApply: (slug: string) =>
      client.call('experts.hooks_apply', { slug }) as Promise<{
        status: 'enabled' | 'disabled' | 'pending_approval' | 'denied' | 'expired';
        files?: number;
        approval_id?: string;
      }>,
    /** Built-in industry packs available for one-click install. Fail-safe:
     *  `deployed: false` when this install ships no premium templates. */
    catalog: () =>
      client.call('experts.catalog') as Promise<{
        deployed: boolean;
        unlocked: boolean;
        present_but_locked: boolean;
        packs: ExpertCatalogEntry[];
      }>,
    /** Convert (cached) + install one built-in industry pack. Long-running:
     *  the full install security pipeline runs inside. */
    installBuiltin: (target: { industry?: string; slug?: string }, attachUnder?: string) =>
      client.call(
        'experts.install_builtin',
        { ...target, ...(attachUnder ? { attach_under: attachUnder } : {}) },
        false,
        320000,
      ) as Promise<{
        success: boolean;
        slug: string;
        output: string;
      }>,
    /** LLM-generate a custom expert-pack draft from the guided form. */
    generate: (req: {
      industry_hint?: string;
      description: string;
      team_size?: number;
      channels?: string[];
    }) => client.call('experts.generate', req, false, 320000) as Promise<ExpertDraftResult>,
    /** Regenerate a draft with feedback (max 5 total rounds). */
    generateRevise: (draftId: string, feedback: string) =>
      client.call(
        'experts.generate_revise',
        { draft_id: draftId, feedback },
        false,
        320000,
      ) as Promise<ExpertDraftResult>,
    /** Install a generated draft via the full security-scanned pipeline. */
    installDraft: (draftId: string, attachUnder?: string) =>
      client.call(
        'experts.install_draft',
        { draft_id: draftId, ...(attachUnder ? { attach_under: attachUnder } : {}) },
        false,
        320000,
      ) as Promise<{
        success: boolean;
        output: string;
      }>,
  },
  gallery: {
    /** Curated inspiration-gallery cards. Fail-safe: `deployed: false` when
     *  this install ships no premium templates or no team ever authors/
     *  derives a non-empty example list. Same license gate as
     *  `experts.catalog` — `present_but_locked` when the tree is deployed
     *  but the license doesn't unlock it. */
    list: () =>
      client.call('gallery.list') as Promise<{
        deployed: boolean;
        unlocked: boolean;
        present_but_locked: boolean;
        cards: GalleryCard[];
      }>,
  },
  /** Agent Mail (P2-d) — the AI staff member's non-real-time mailbox.
   *  Manager-gated, same tier as the approval centre. `decide` does not send:
   *  it records the human decision, and the gateway's mail worker performs
   *  (or refuses) the transmission on its next pass. */
  mail: {
    status: () => client.call('mail.status') as Promise<MailStatus>,
    list: (params: { agent_id?: string; include_archived?: boolean; limit?: number } = {}) =>
      client.call('mail.list', params) as Promise<{ count: number; messages: MailMessage[] }>,
    /** Reads one message in full — and marks it read as a side effect. */
    read: (mail_id: string) => client.call('mail.read', { mail_id }) as Promise<MailMessageFull>,
    archive: (mail_id: string) => client.call('mail.archive', { mail_id }) as Promise<{ ok: boolean }>,
    outbox: (params: { agent_id?: string; status?: MailDraft['status']; limit?: number } = {}) =>
      client.call('mail.outbox', params) as Promise<{ count: number; drafts: MailDraft[] }>,
    /** Confirm (`approve: true`) or refuse an outgoing draft. `note` is an
     *  optional human explanation — most useful on a refusal, so the AI
     *  employee's next draft can address why. Sent whenever non-empty
     *  regardless of the decision; the gateway is the authority on whether a
     *  given decision path persists it. */
    decide: (mail_id: string, approve: boolean, note?: string) =>
      client.call('mail.decide', {
        mail_id,
        approve,
        ...(note && note.trim() ? { note: note.trim() } : {}),
      }) as Promise<{
        ok: boolean;
        mail_id: string;
        approved: boolean;
        state: 'approved_queued' | 'rejected';
      }>,
  },
  channels: {
    status: () =>
      client.call('channels.status') as Promise<{ channels: ChannelStatus[] }>,
    add: (type: string, config: Record<string, string>, agent?: string) =>
      client.call('channels.add', { type, config, ...(agent ? { agent } : {}) }),
    // W0-2: the gateway actually sends a test message when it can find a
    // destination (`mode: "live"`); when no destination is known yet it
    // honestly degrades to `mode: "credential_only"` (only verified the
    // token exists — never reported as a successful send).
    test: (type: string) =>
      client.call('channels.test', { type }) as Promise<{
        sent: boolean;
        mode: 'live' | 'credential_only';
        detail: string;
      }>,
    remove: (type: string) =>
      client.call('channels.remove', { type }),
    // WP1.1 (ecosystem): LINE OA add-friend link for the QR onboarding card /
    // printable poster. QR is rendered client-side from `add_friend_url`.
    lineAddFriend: () =>
      client.call('channels.line_add_friend', {}) as Promise<{
        add_friend_url: string;
        basic_id: string;
        display_name: string | null;
      }>,
    // WP9: mint a one-time Telegram deep-link/QR bind token so an employee can
    // bind the company's shared bot to a specific AI employee (agent).
    telegramBindToken: (agent: string, opts?: { ttl_minutes?: number; max_uses?: number }) =>
      client.call('channels.telegram_bind_token', { agent, ...(opts ?? {}) }) as Promise<{
        agent: string;
        token: string;
        bot_username: string;
        deep_link: string;
        expires_in_minutes: number;
        max_uses: number;
      }>,
    // W2-2 (E1) — behavior settings ("行為" tab). Reads/writes the same
    // `ChannelSettingsManager` the `channel_config` MCP tool uses.
    configGet: (channel: string, scopeId?: string) =>
      client.call('channels.config_get', {
        channel,
        ...(scopeId ? { scope_id: scopeId } : {}),
      }) as Promise<{
        success: boolean;
        channel: string;
        scope_id: string;
        settings: ChannelConfigSettings;
        /** Other known scopes (e.g. Discord guild ids) for this channel type. */
        scopes: string[];
      }>,
    configSet: (channel: string, settings: Partial<ChannelConfigSettings>, scopeId?: string) =>
      client.call('channels.config_set', {
        channel,
        ...(scopeId ? { scope_id: scopeId } : {}),
        settings,
      }) as Promise<{
        success: boolean;
        channel: string;
        scope_id: string;
        changes: Array<{ key: string; value: unknown }>;
      }>,
    // W2-2 (E2) — access-control settings ("存取" tab). Always the "global"
    // scope (the only scope the gateway's access gate actually reads).
    accessGet: (channel: string) =>
      client.call('channels.access_get', { channel }) as Promise<{
        success: boolean;
        channel: string;
        settings: ChannelAccessSettings;
      }>,
    accessSet: (channel: string, settings: Partial<ChannelAccessSettings>) =>
      client.call('channels.access_set', { channel, settings }) as Promise<{
        success: boolean;
        channel: string;
        scope_id: string;
        changes: Array<{ key: string; value: unknown }>;
      }>,
    // Approved pairing subjects (`/pair <code>` redemptions) — shared across
    // channel types, mirroring the `pairing_manage` MCP tool's storage.
    pairingList: () =>
      client.call('channels.pairing_list', {}) as Promise<{
        success: boolean;
        approved: string[];
      }>,
    pairingRevoke: (subject: string) =>
      client.call('channels.pairing_revoke', { subject }) as Promise<{
        success: boolean;
        subject: string;
        revoked: boolean;
      }>,
  },
  // Interactive CLI login ("Dashboard 一鍵登入") — drives a CLI's native login
  // in a PTY on the gateway and streams it back via `auth.cli_login.*` events.
  auth: {
    cliLoginStart: (runtime: 'claude' | 'codex' | 'gemini' | 'antigravity' | 'grok') =>
      client.call('auth.cli_login.start', { runtime }) as Promise<{
        session_id: string;
        runtime: string;
        program: string;
        remote_safe: boolean;
        hint: string;
        status: string;
      }>,
    cliLoginInput: (sessionId: string, data: string) =>
      client.call('auth.cli_login.input', { session_id: sessionId, data }) as Promise<{
        success: boolean;
      }>,
    cliLoginStatus: (sessionId: string) =>
      client.call('auth.cli_login.status', { session_id: sessionId }) as Promise<{
        session_id: string;
        status: 'running' | 'succeeded' | 'failed' | 'exited';
      }>,
    cliLoginCancel: (sessionId: string) =>
      client.call('auth.cli_login.cancel', { session_id: sessionId }) as Promise<{
        success: boolean;
      }>,
    // Register the account a successful one-click login produced (scrapes the
    // long-lived OAuth token the CLI printed and writes it to config).
    cliLoginFinalize: (sessionId: string) =>
      client.call('auth.cli_login.finalize', { session_id: sessionId }) as Promise<{
        registered: boolean;
        account_id?: string;
        /** 'cli_store' = credentials persisted to the CLI's own store — a
         *  success for non-Claude CLIs (no [[accounts]] entry needed). */
        reason?: string;
        /** Present when reason === 'cli_store': the credential file path. */
        store?: string;
      }>,
  },

  accounts: {
    list: () =>
      client.call('accounts.list') as Promise<{ accounts: AccountInfo[] }>,
    budgetSummary: () =>
      client.call('accounts.budget_summary') as Promise<BudgetSummary>,
    // WP-D: "訂閱帳號" device-code-style setup wizard — a guided, single-
    // flight, pre-validated specialization of the auth.cliLogin* flow above,
    // scoped to Claude subscription accounts. See setup_token_wizard.rs.
    setupTokenStart: () =>
      client.call('accounts.setup_token_start') as Promise<{
        session_id: string;
        /** `null` until the CLI has printed it — poll `setupTokenStatus`. */
        auth_url: string | null;
        expires_in_seconds: number;
        program: string;
      }>,
    setupTokenStatus: (sessionId: string) =>
      client.call('accounts.setup_token_status', { session_id: sessionId }) as Promise<{
        session_id: string;
        status: 'running' | 'succeeded' | 'failed' | 'exited';
        auth_url: string | null;
        expires_in_seconds: number;
      }>,
    setupTokenCancel: (sessionId: string) =>
      client.call('accounts.setup_token_cancel', { session_id: sessionId }) as Promise<{
        success: boolean;
      }>,
    // Rejects with a structured `{code, message}` — code is one of
    // 'not_installed' | 'no_active_session' | 'expired' | 'invalid_code' |
    // 'timeout' | 'validation_failed' | 'already_submitting' | 'io_error'.
    setupTokenSubmit: (sessionId: string, code: string) =>
      client.call('accounts.setup_token_submit', { session_id: sessionId, code }) as Promise<{
        success: boolean;
        account_id: string;
      }>,
    /** CLI-store credentials (grok/codex/gemini) — subscription logins that
     *  live in the CLI's own credential file, not in rotator accounts. */
    cliCredentials: () =>
      client.call('accounts.cli_credentials') as Promise<{
        credentials: CliCredentialInfo[];
      }>,
    rotate: () =>
      client.call('accounts.rotate') as Promise<{ success: boolean }>,
    health: () =>
      client.call('accounts.health') as Promise<Record<string, unknown>>,
    updateBudget: (accountId: string, monthlyBudgetCents: number) =>
      client.call('accounts.update_budget', {
        account_id: accountId,
        monthly_budget_cents: monthlyBudgetCents,
      }) as Promise<{ success: boolean }>,
    add: (params: { id: string; type: string; key: string; monthly_budget_cents: number; priority: number }) =>
      client.call('accounts.add', params) as Promise<{ success: boolean }>,
    /** G.5 — general per-account edit (no secret). Send only changed fields. */
    update: (params: {
      account_id: string;
      priority?: number;
      tags?: string[];
      profile?: string;
      email?: string;
      subscription?: string;
      label?: string;
      monthly_budget_cents?: number;
    }) =>
      client.call('accounts.update', params) as Promise<{ success: boolean; changes: string[] }>,
  },
  memory: {
    /**
     * WP15 — both memory reads come back pre-split by the gateway:
     * `entries` is what the user told the agent, `signals` is the platform's
     * own learning telemetry (prediction deviations, mood snapshots). The
     * split is done server-side so each list has its own row budget; see
     * `duduclaw_memory::is_system_signal` for the classification.
     */
    search: (agentId: string, query: string, limit = 20) =>
      client.call('memory.search', {
        agent_id: agentId,
        query,
        limit,
      }) as Promise<{ entries: MemoryEntry[]; signals?: MemoryEntry[] }>,
    browse: (agentId: string, limit = 20) =>
      client.call('memory.browse', {
        agent_id: agentId,
        limit,
      }) as Promise<{ entries: MemoryEntry[]; signals?: MemoryEntry[] }>,
    keyFacts: (agentId: string, limit = 50) =>
      client.call('memory.key_facts', {
        agent_id: agentId,
        limit,
      }) as Promise<{ entries: KeyFactEntry[] }>,
    /**
     * Read-only aggregate behind the memory-health view: how fresh this
     * agent's memories are overall, which ones are closest to being filed
     * away, which ones keep getting recalled, and how the pile has grown over
     * `days`. Derived from the same decay curve as the archival job.
     */
    decayOverview: (agentId: string, days = 30, topN = 5) =>
      client.call('memory.decay_overview', {
        agent_id: agentId,
        days,
        top_n: topN,
      }) as Promise<MemoryDecayOverview>,
    /** Supersession chain for a fact — by (subject, predicate) or memory_id. */
    history: (agentId: string, query: MemoryHistoryQuery) =>
      client.call('memory.history', {
        agent_id: agentId,
        ...query,
      }) as Promise<MemoryHistoryResult>,
    /** Point-in-time lookup: which value was valid for (subject, predicate) at `at`. */
    at: (agentId: string, subject: string, predicate: string, at: string) =>
      client.call('memory.at', {
        agent_id: agentId,
        subject,
        predicate,
        at,
      }) as Promise<MemoryAtResult>,
    /** D6 — export the agent's SPO knowledge graph for the curation viewer. */
    graph: (agentId: string, limit = 500) =>
      client.call('memory.graph', {
        agent_id: agentId,
        limit,
      }) as Promise<MemoryGraphResult>,
    /**
     * D6 — DESTRUCTIVE: expire every currently-valid fact from one source.
     * Dashboard-local only. `since` (RFC-3339) optionally bounds by learn time.
     */
    invalidateOrigin: (agentId: string, origin: string, since?: string) =>
      client.call('memory.invalidate_origin', {
        agent_id: agentId,
        origin,
        ...(since ? { since } : {}),
      }) as Promise<{ expired: number }>,
    /**
     * Forget one entry. Soft delete — the backend archives the row before
     * removing it from search / browse / prompt injection, so an operator can
     * still recover it. `forgotten:false` = the id was already gone.
     */
    forget: (agentId: string, memoryId: string) =>
      client.call('memory.forget', {
        agent_id: agentId,
        memory_id: memoryId,
      }) as Promise<{ success: boolean; forgotten: boolean }>,
  },
  wiki: {
    pages: (agentId: string) =>
      client.call('wiki.pages', { agent_id: agentId }) as Promise<{ pages: WikiPageMeta[]; exists: boolean }>,
    read: (agentId: string, pagePath: string) =>
      client.call('wiki.read', { agent_id: agentId, page_path: pagePath }) as Promise<{ content: string; path: string }>,
    search: (agentId: string, query: string, limit = 10) =>
      client.call('wiki.search', { agent_id: agentId, query, limit }) as Promise<{ hits: WikiSearchHit[] }>,
    /** WP5c — pages the AI filed on its own, with their conversation source chain. */
    autoPages: (agentId: string) =>
      client.call('wiki.auto_pages', { agent_id: agentId }) as Promise<{
        pages: AutoWikiPage[];
        exists: boolean;
      }>,
    /** Confirm an auto-filed page as curated knowledge (raises trust, starts injecting). */
    promote: (agentId: string, pagePath: string) =>
      client.call('wiki.promote', { agent_id: agentId, page_path: pagePath }) as Promise<{
        promoted: boolean;
        path: string;
      }>,
    /** Remove one auto-filed page and expire exactly its memory pointer. */
    archive: (agentId: string, pagePath: string) =>
      client.call('wiki.archive', { agent_id: agentId, page_path: pagePath }) as Promise<{
        archived: boolean;
        pointers_expired: number;
        path: string;
      }>,
    /** Copy an auto-filed page into the shared knowledge base. */
    share: (agentId: string, pagePath: string) =>
      client.call('wiki.share', { agent_id: agentId, page_path: pagePath }) as Promise<{
        shared: boolean;
        path: string;
      }>,
    lint: (agentId: string) =>
      client.call('wiki.lint', { agent_id: agentId }) as Promise<WikiLintReport>,
    stats: (agentId: string) =>
      client.call('wiki.stats', { agent_id: agentId }) as Promise<WikiStats>,
    trustAudit: (agentId: string, maxTrust = 0.3, limit = 50) =>
      client.call('wiki.trust_audit', { agent_id: agentId, max_trust: maxTrust, limit }) as Promise<{
        rows: WikiTrustRow[];
        available: boolean;
        note?: string;
      }>,
    trustHistory: (agentId: string, pagePath: string, limit = 50) =>
      client.call('wiki.trust_history', { agent_id: agentId, page_path: pagePath, limit }) as Promise<{
        rows: WikiTrustHistoryRow[];
        available: boolean;
      }>,
    trustOverride: (params: {
      agent_id: string;
      page_path: string;
      trust: number;
      lock?: boolean;
      do_not_inject?: boolean;
      reason?: string;
    }) =>
      client.call('wiki.trust_override', params) as Promise<{
        page_path: string;
        agent_id: string;
        old_trust: number;
        new_trust: number;
        applied_delta: number;
        locked: boolean;
        became_archived: boolean;
        became_recovered: boolean;
      }>,
  },
  sharedWiki: {
    pages: () =>
      client.call('shared_wiki.pages') as Promise<{ pages: WikiPageMeta[]; exists: boolean }>,
    read: (pagePath: string) =>
      client.call('shared_wiki.read', { page_path: pagePath }) as Promise<{ content: string; path: string }>,
    search: (query: string, limit = 10) =>
      client.call('shared_wiki.search', { query, limit }) as Promise<{ hits: WikiSearchHit[] }>,
    stats: () =>
      client.call('shared_wiki.stats') as Promise<SharedWikiStats>,
  },
  skills: {
    list: (agentId: string) =>
      client.call('skills.list', { agent_id: agentId }) as Promise<{
        skills: SkillInfo[];
        scanned?: SkillScanPath[];
      }>,
    /** Aggregate across every layer and every staffer (no `agent_id`). */
    listAll: () => client.call('skills.list', {}) as Promise<SkillsListAll>,
    content: (agentId: string, skillName: string) =>
      client.call('skills.content', {
        agent_id: agentId,
        skill_name: skillName,
      }) as Promise<{ content: string }>,
    vet: (url: string) =>
      client.call('skills.vet', { url }) as Promise<{
        skill_name: string;
        content: string;
        vet_result: { passed: boolean; findings: Array<{ severity: string; category: string; description: string }>; score: number };
        passed: boolean;
      }>,
    install: (url: string, scope: string, content: string) =>
      client.call('skills.install', { url, scope, content }) as Promise<{
        success: boolean;
        skill_name: string;
        scope: string;
      }>,
    /** Non-admin: file a Skill install request for the manager→admin chain. */
    installRequest: (url: string, scope: string, content: string) =>
      client.call('skills.install_request', { url, scope, content }) as Promise<InstallRequestFiled>,
    /** WP10-T10.1 — approved skills ranked by estimated time saved. Any authed. */
    leaderboard: (limit?: number) =>
      client.call('skills.leaderboard', limit != null ? { limit } : {}) as Promise<{
        leaderboard: SkillLeaderboardEntry[];
        metric: string;
        note: string;
      }>,
  },
  /** Local-model marketplace (design: DESIGN-local-model-marketplace).
   *  Reads for any user; install/cancel/remove are manager+. */
  localmodels: {
    search: (intent: string) =>
      client.call('localmodels.search', { intent }) as Promise<{
        models: MarketModel[];
        hardware: MarketHardware;
      }>,
    quants: (repo: string) =>
      client.call('localmodels.quants', { repo }) as Promise<{ model: MarketModel }>,
    installed: () =>
      client.call('localmodels.installed') as Promise<{
        models: Array<{ filename: string; size_bytes: number }>;
      }>,
    install: (repo: string, filename: string, shards: string[], totalBytes: number) =>
      client.call('localmodels.install', {
        repo,
        filename,
        shards,
        total_bytes: totalBytes,
      }) as Promise<{ job_id: number }>,
    installStatus: () =>
      client.call('localmodels.install_status') as Promise<{ jobs: MarketInstallJob[] }>,
    cancel: (jobId: number) =>
      client.call('localmodels.cancel', { job_id: jobId }) as Promise<{ cancelled: boolean }>,
    remove: (filename: string) =>
      client.call('localmodels.remove', { filename }) as Promise<{ removed: boolean }>,
  },
  /** v1.53/54 task forward-model + calibration views — generic, per-agent
   *  (the LWM trading experiment is just one producer of this store). */
  forward: {
    summary: (agentId?: string) =>
      client.call('forward.summary', { agent_id: agentId ?? '' }) as Promise<{
        agents: ForwardAgentSummary[];
        window_scanned: number;
        window_cap: number;
      }>,
    recent: (agentId?: string, limit = 50) =>
      client.call('forward.recent', { agent_id: agentId ?? '', limit }) as Promise<{
        predictions: ForwardPredictionRow[];
      }>,
    /** Every round of one task's predict→act→observe→score loop. */
    chain: (taskId: string) =>
      client.call('forward.chain', { task_id: taskId }) as Promise<{ rounds: ForwardChainRound[] }>,
    /** Query-time skill verdict (Brier / Murphy / reliability bins / label). */
    calibration: (agentId: string) =>
      client.call('forward.calibration', { agent_id: agentId }) as Promise<{
        calibration: ForwardCalibration;
      }>,
    /** Learned state buckets, most-sampled first. */
    states: (agentId?: string, limit = 20) =>
      client.call('forward.states', { agent_id: agentId ?? '', limit }) as Promise<{
        states: ForwardStateRow[];
      }>,
  },
  /** Belief Loop — external-world prediction bookkeeping,
   *  parallel to `api.forward` above (design-market-belief-loop-2026-08). */
  belief: {
    recent: (agentId?: string, limit = 50) =>
      client.call('belief.recent', { agent_id: agentId ?? '', limit }) as Promise<{
        beliefs: BeliefRow[];
      }>,
    summary: (agentId: string) =>
      client.call('belief.summary', { agent_id: agentId }) as Promise<{ stats: BeliefStats }>,
  },
  /** Security audit dashboard (DESIGN-code-security-audit-2026-08 §3.1) —
   *  reports written by `duduclaw secaudit --save`. Manager+ gated
   *  server-side (`require_manager!()`), same bar as `forward`/`belief`. */
  secaudit: {
    reports: () =>
      client.call('secaudit.reports') as Promise<{ reports: SecauditReportRow[] }>,
    report: (file: string) =>
      client.call('secaudit.report', { file }) as Promise<{ report: SecauditReport }>,
    /** Operator confirm/suppress/refute decision on one finding. Returns the
     *  server's updated finding object so the caller can reconcile its
     *  optimistic update rather than assume the write matched intent. */
    findingStatus: (file: string, findingId: string, status: SecauditFindingStatus) =>
      client.call('secaudit.finding_status', {
        file,
        finding_id: findingId,
        status,
      }) as Promise<{ success: boolean; file: string; finding: SecauditFinding }>,
  },
  evolution: {
    status: () =>
      client.call('evolution.status') as Promise<{
        enabled: boolean;
        mode: string;
        total_agents: number;
        gvu_enabled_count: number;
        total_versions: number;
        last_applied_at: string | null;
        agents: Array<{
          agent_id: string;
          gvu_enabled: boolean;
          cognitive_memory: boolean;
          skill_auto_activate: boolean;
          skill_security_scan: boolean;
          max_silence_hours: number;
          max_gvu_generations: number;
          observation_period_hours: number;
        }>;
      }>,
    history: (agentId?: string, limit = 20) =>
      client.call('evolution.history', { agent_id: agentId ?? '', limit }) as Promise<{
        versions: EvolutionVersion[];
      }>,
    /** Superset of `history`: same optional `agent_id`/`limit` scoping, plus
     *  the WP0.4 `ExpiredNoData` status and the one-time low-data alert flag. */
    versions: (agentId?: string, limit = 20) =>
      client.call('evolution.versions', { agent_id: agentId ?? '', limit }) as Promise<{
        versions: EvolutionVersion[];
      }>,
    /** AVO §2.4 stagnation detector snapshot. Empty `agentId` scopes to every
     *  registered agent (one snapshot per agent, in registry order). */
    stagnation: (agentId?: string) =>
      client.call('evolution.stagnation', { agent_id: agentId ?? '' }) as Promise<{
        snapshots: EvolutionStagnationSnapshot[];
      }>,
    /** WP0.6 Verifier/Updater rejection distribution over a trailing window. */
    telemetry: (agentId?: string, days = 7) =>
      client.call('evolution.telemetry', { agent_id: agentId ?? '', days }) as Promise<EvolutionTelemetrySummary>,
    /** WP0.2 consolidation (whole-SOUL.md-rewrite) attempt audit trail. */
    consolidations: (agentId?: string, limit = 20) =>
      client.call('evolution.consolidations', { agent_id: agentId ?? '', limit }) as Promise<{
        consolidations: EvolutionConsolidation[];
      }>,
  },
  /** Playbook — gene-shaped experience entries the AEE evolution loop writes
   *  instead of rewriting SOUL.md (TODO-evolution-v3-2026-08.md §Phase 1). */
  playbook: {
    list: (agentId: string) =>
      client.call('playbook.list', { agent_id: agentId }) as Promise<{
        agent_id: string;
        entries: PlaybookEntry[];
      }>,
    /** Human-initiated terminal retirement of one entry. */
    retire: (agentId: string, id: string, reason?: string) =>
      client.call('playbook.retire', { agent_id: agentId, id, reason: reason ?? '' }) as Promise<{
        success: boolean;
        retired: boolean;
        reason?: string;
      }>,
    /** Lossless GEP-gene-shaped JSON export (local schema alignment, D5=B — no hub I/O). */
    export: (agentId: string) =>
      client.call('playbook.export', { agent_id: agentId }) as Promise<{
        agent_id: string;
        gene_schema: string;
        genes: unknown[];
      }>,
  },
  /** Delegation permissions (WP21 §2.8) — owner/admin only, gateway-side gated.
   *  `allow` holds unordered agent-id pairs; a pair means the two may hand work
   *  to each other in both directions. */
  delegation: {
    get: () =>
      client.call('delegation.get') as Promise<DelegationSettings>,
    set: (params: { policy?: DelegationPolicy; allow?: Array<[string, string]> }) =>
      client.call('delegation.set', params) as Promise<{
        success: boolean;
        policy: DelegationPolicy;
        allow: Array<[string, string]>;
        applied: boolean;
      }>,
  },
  /** v1.54 校準式預測 + held-out 學習閘 全域開關 (admin only). */
  taskForwardModel: {
    get: () =>
      client.call('task_forward_model.get') as Promise<TaskForwardModelSettings>,
    set: (params: Partial<TaskForwardModelSettings>) =>
      client.call('task_forward_model.set', params) as Promise<
        TaskForwardModelSettings & { success: boolean; enabled_requires_restart: boolean }
      >,
  },
  system: {
    status: () =>
      client.call('system.status') as Promise<SystemStatus>,
    /** Login/boot autostart registration state (admin). */
    autostartStatus: () =>
      client.call('system.autostart.status') as Promise<AutostartStatus>,
    /** Register/unregister the gateway to start at login/boot (admin).
     *  Registration only — never starts or stops the running gateway. */
    autostartSet: (enabled: boolean) =>
      client.call('system.autostart.set', { enabled }) as Promise<AutostartStatus>,
    doctor: () =>
      // Extended timeout: the gateway runs live probes concurrently (MCP
      // server cold-start ~10s, grok -p ping ~15s+version 5s) — worst case
      // ~20s, above the 30s default's comfort zone once queueing is added.
      client.call('system.doctor', {}, false, 60000) as Promise<{
        checks: DoctorCheck[];
        summary: { pass: number; warn: number; fail: number };
      }>,
    doctorRepair: () =>
      client.call('system.doctor_repair'),
    version: () =>
      client.call('system.version') as Promise<{
        version: string;
        auto_update: boolean;
        edition: string;
        edition_profile?: EditionProfile;
      }>,
    config: () =>
      client.call('system.config') as Promise<{
        config?: string;
        voice?: Partial<VoiceSettings> | null;
        // Structured [gateway] allowed_origins for the remote-access allowlist UI.
        allowed_origins?: string[];
        // Structured [skills] gap_digest_enabled for the daily skill-gap digest toggle.
        gap_digest_enabled?: boolean;
        // Structured [memory] novelty_gate for the memory-dedup-gate toggle
        // (absent ⇒ true, matching the gateway's fail-closed default).
        novelty_gate_enabled?: boolean;
        // Structured [notify] daily_digest for the daily-digest toggle
        // (W2-8; absent ⇒ false/"09:00", matching `DigestConfig::default()`).
        daily_digest_enabled?: boolean;
        daily_digest_at?: string;
      }>,
    updateConfig: (fields: Record<string, unknown>) =>
      client.call('system.update_config', fields) as Promise<{ success: boolean; changes: string[]; applied?: boolean; hot_reloaded?: string[] }>,
    checkUpdate: () =>
      client.call('system.check_update') as Promise<{
        available: boolean;
        current_version: string;
        latest_version: string;
        release_notes: string;
        published_at: string;
        download_url: string;
        install_method: string;
        auto_update: boolean;
        /** Non-null when a newer binary is already on disk — restart to apply. */
        restart_pending_version?: string | null;
        /**
         * Which channel can actually install this update:
         * - `control_plane` — an enterprise update provider is configured.
         * - `github` — the public release channel (CE).
         * - `none` — enterprise binary with no update channel wired up: the new
         *   version is real, but nothing here can install it, so the install
         *   button must not be offered.
         */
        update_channel?: 'control_plane' | 'github' | 'none';
        /**
         * True when this gateway is running inside a container (`/.dockerenv`
         * or `DUDUCLAW_IN_CONTAINER`). The image is immutable — an in-process
         * binary swap never sticks — so the dashboard shows an `update.sh` /
         * image-rebuild guidance card instead of an install button. Absent on
         * older gateways ⇒ treated as `false`.
         */
        containerized?: boolean;
      }>,
    /**
     * Installing an update = download + checksum + signature + extract + swap,
     * and the download stage now retries transient failures twice (5s / 15s
     * back-off, `updater::DOWNLOAD_RETRY_DELAYS_SECS`) on top of a 300s HTTP
     * timeout per attempt. The default 30s RPC timeout would abandon a request
     * the gateway is still working on and paint the button red while the
     * install actually completed — the shape of the 2026-08-04 "failed once,
     * worked on retry" field report. 10 minutes covers the worst legitimate
     * case; the gateway's own timeouts still bound it from the other side.
     */
    applyUpdate: () =>
      client.call('system.apply_update', {}, false, 600000) as Promise<{
        success: boolean;
        message: string;
        needs_restart: boolean;
      }>,
  },
  cron: {
    list: () =>
      client.call('cron.list') as Promise<{
        tasks: Array<{
          id: string;
          name?: string;
          agent_id: string;
          cron: string;
          schedule?: string;
          task?: string;
          enabled: boolean;
          last_run_at?: string | null;
          last_status?: string | null;
        }>;
      }>,
    add: (params: { name: string; agent_id: string; cron: string; task?: string }) =>
      client.call('cron.add', params),
    update: (
      id: string,
      params: {
        name?: string;
        agent_id?: string;
        cron?: string;
        task?: string;
        enabled?: boolean;
      }
    ) => client.call('cron.update', { id, ...params }),
    pause: (id: string) =>
      client.call('cron.pause', { id }),
    resume: (id: string) =>
      client.call('cron.resume', { id }),
    remove: (id: string) =>
      client.call('cron.remove', { id }),
    /** Trigger a single immediate ("test") execution of a routine. */
    runNow: (id: string) =>
      client.call('cron.run_now', { id }) as Promise<{
        success: boolean;
        id: string;
        name: string;
      }>,
    /** Built-in office scheduling templates for prefilling the create dialog. */
    templates: () =>
      client.call('cron.templates') as Promise<{
        templates: Array<{
          id: string;
          name: string;
          cron: string;
          description: string;
          prompt: string;
        }>;
      }>,
  },
  heartbeat: {
    status: () =>
      client.call('heartbeat.status') as Promise<{ heartbeats: HeartbeatInfo[] }>,
    trigger: (agentId: string) =>
      client.call('heartbeat.trigger', { agent_id: agentId }) as Promise<{ success: boolean }>,
  },
  security: {
    auditLog: (limit = 50) =>
      client.call('security.audit_log', { limit }) as Promise<{ events: AuditEvent[] }>,
    status: () =>
      client.call('security.status') as Promise<{
        credential_proxy: { active: boolean; vault_backend: string; injected_secrets: number };
        mount_guard: { rules: Array<{ path: string; access: string }> };
        rbac: Array<{
          agent_id: string; role: string;
          tool_use: boolean; web_access: boolean;
          file_write: boolean; shell_exec: boolean; delegate: boolean;
        }>;
        rate_limiter: { requests_per_minute: number; concurrent_requests: number };
        soul_drift: Array<{ agent_id: string; soul_exists: boolean; gvu_enabled: boolean }>;
      }>,
    /** Read-only scan of config.toml for plaintext credentials — paths only,
     *  never values (WP-K). */
    credentialHygiene: () =>
      client.call('security.credential_hygiene') as Promise<CredentialHygieneReport>,
    /** Removes ONLY plaintext fields that already have a confirmed `_enc`
     *  twin; auto-backs up config.toml first. Fields with no twin are left
     *  for manual handling (see `CredentialFinding.has_enc_twin`). */
    credentialCleanup: () =>
      client.call('security.credential_cleanup') as Promise<CredentialCleanupResult>,
    /** Every credential field with its source verdict — no values, and no
     *  backend round-trips (WP-H1 P1). */
    credentialInventory: () =>
      client.call('security.credential_inventory') as Promise<CredentialInventoryReport>,
  },
  audit: {
    unifiedLog: (params?: {
      limit?: number;
      sources?: UnifiedAuditSource[];
      severity_filter?: 'info' | 'warning' | 'critical';
      agent_id_filter?: string;
    }) => client.call('audit.unified_log', params ?? {}) as Promise<UnifiedAuditResponse>,
    reliabilitySummary: (agentId: string, windowDays = 7) =>
      client.call('audit.reliability_summary', {
        agent_id: agentId,
        window_days: windowDays,
      }) as Promise<ReliabilitySummary>,
    evolutionQuery: (filter: EvolutionQueryFilter = {}) =>
      client.call('audit.evolution_query', { ...filter }) as Promise<EvolutionQueryResult>,
  },
  tools: {
    catalog: () =>
      client.call('tools.catalog') as Promise<{ tools: ToolCatalogEntry[] }>,
    /** Built-in tool catalog for the capability editor picker. Readable by any
     *  authenticated user (same tier as `agents.list`). */
    builtinCatalog: () =>
      client.call('tools.builtin_catalog') as Promise<{ tools: BuiltinToolEntry[] }>,
  },
  skillMarket: {
    /** `total_indexed === 0` means the market index itself never loaded (the
     *  GitHub refresh is best-effort and swallows its error) — a very
     *  different message than "your query matched nothing". */
    search: (query: string) =>
      client.call('skills.search', { query }) as Promise<{
        skills: SkillIndexEntry[];
        source?: string;
        total_indexed?: number;
      }>,
  },
  models: {
    list: () =>
      client.call('models.list') as Promise<{
        models: Array<{
          id: string;
          label: string;
          type: 'cloud' | 'local';
          provider?: string;
          /** Discovery source: live_api / cli_probe / help_parse / pty_probe / fallback. */
          source?: string;
          /** RFC3339 timestamp of the last probe for this provider. */
          fetched_at?: string;
          file?: string;
          size_bytes?: number;
        }>;
        default_local: string | null;
        /** RFC3339 timestamp of the whole discovery run. */
        discovered_at?: string;
      }>,
    /** Force a live re-probe of every installed CLI/API, then return the fresh list. */
    refresh: () =>
      client.call('models.refresh') as Promise<{
        models: Array<{
          id: string;
          label: string;
          type: 'cloud' | 'local';
          provider?: string;
          source?: string;
          fetched_at?: string;
          file?: string;
          size_bytes?: number;
        }>;
        default_local: string | null;
        discovered_at?: string;
      }>,
  },
  logs: {
    subscribe: () =>
      client.call('logs.subscribe'),
    unsubscribe: () =>
      client.call('logs.unsubscribe'),
  },
  browser: {
    auditLog: (limit = 20, agentId?: string) =>
      client.call('browser.audit_log', { limit, agent_id: agentId }) as Promise<{ entries: BrowserAuditEntry[] }>,
    emergencyStop: (action: 'status' | 'stop' | 'resume') =>
      client.call('browser.emergency_stop', { action }) as Promise<{ status: 'normal' | 'stopped' | 'unknown' }>,
    toolApprove: (
      action: 'list' | 'approve' | 'revoke',
      params?: {
        agent_id?: string;
        tool_name?: string;
        duration_minutes?: number;
        session_scoped?: boolean;
      }
    ) =>
      client.call('browser.tool_approve', { action, ...params }) as Promise<{ approvals: ToolApproval[] }>,
    browserbaseSessions: (
      action: 'list' | 'create' | 'close',
      params?: { limit?: number; session_id?: string }
    ) =>
      client.call('browser.browserbase_sessions', { action, ...params }) as Promise<{ sessions: BrowserbaseSession[] }>,
    browserbaseCost: (hours = 24) =>
      client.call('browser.browserbase_cost', { hours }) as Promise<BrowserbaseCostSummary>,
  },
  analytics: {
    summary: (period: 'day' | 'week' | 'month') =>
      client.call('analytics.summary', { period }) as Promise<{
        total_conversations: number;
        total_messages: number;
        auto_reply_rate: number;
        avg_response_ms: number;
        p95_response_ms: number;
        zero_cost_ratio: number;
        estimated_savings_cents: number;
        period: string;
      }>,
    conversations: () =>
      client.call('analytics.conversations') as Promise<{
        daily: Array<{ date: string; count: number; auto_count: number }>;
      }>,
    costSavings: () =>
      client.call('analytics.cost_savings') as Promise<{
        monthly: Array<{ month: string; human_cost: number; agent_cost: number; savings: number }>;
      }>,
  },
  // Cache-efficiency telemetry (CostTelemetry). `available:false` on every
  // response when telemetry isn't initialized.
  cost: {
    summary: (hours = 24) =>
      client.call('cost.summary', { hours }) as Promise<CostSummary>,
    agents: (hours = 24) =>
      client.call('cost.agents', { hours }) as Promise<CostAgentsResult>,
    recent: (limit = 20) =>
      client.call('cost.recent', { limit }) as Promise<CostRecentResult>,
  },
  // Notification governance action-rate telemetry (W2-4/W2-8). Manager+
  // (aggregates across every agent) — same gate as `/reports` itself.
  notify: {
    stats: (days = 30) =>
      client.call('notify.stats', { days }) as Promise<NotifyStatsResponse>,
  },
  billing: {
    usage: () =>
      client.call('billing.usage') as Promise<BillingUsage>,
    history: () =>
      client.call('billing.history') as Promise<{ invoices: BillingInvoice[] }>,
  },
  // WP14-T14.7 — HITL approval center. `list` is manager-gated; `decide` errors
  // on already-terminal requests or board-kind requests without admin scope.
  approvals: {
    /** `actionKind` (e.g. 'knowledge_quarantine') filters to one approval kind. */
    list: (agentId?: string, actionKind?: string) =>
      client.call('approvals.list', {
        ...(agentId ? { agent_id: agentId } : {}),
        ...(actionKind ? { action_kind: actionKind } : {}),
      }) as Promise<{
        approvals: ApprovalItem[];
        count: number;
      }>,
    decide: (id: string, approve: boolean, reason?: string) =>
      client.call('approvals.decide', {
        id,
        approve,
        ...(reason ? { reason } : {}),
      }) as Promise<{
        id: string;
        decided: 'approved' | 'denied';
      }>,
  },
  // W3-1 — read-only: conversations a human currently holds. See
  // `TakeoverListResponse` above for why there's no write twin.
  takeover: {
    list: () => client.call('takeover.list') as Promise<TakeoverListResponse>,
  },
  // WP14-T14.6 — budget incident console (manager-gated read).
  budget: {
    incidents: (limit?: number) =>
      client.call('budget.incidents', limit != null ? { limit } : {}) as Promise<{
        incidents: BudgetIncident[];
        by_agent: BudgetByAgent[];
      }>,
  },
  license: {
    /**
     * Read-only snapshot of the gateway LicenseRuntime. Returns
     * OpenSource defaults when no license is installed, so the caller
     * can render without conditional-loading the call.
     */
    status: () => client.call('license.status') as Promise<LicenseSnapshot>,
    /** Machine fingerprint — customers quote this when purchasing a license. */
    fingerprint: () =>
      client.call('license.fingerprint', {}) as Promise<{ fingerprint: string }>,
    /**
     * Install + hot-reload a license (admin-only). `key` accepts either the
     * base64 key blob or the full license JSON. Errors arrive as WsFrame
     * error strings, already localized zh-TW (bad signature / fingerprint
     * mismatch / expired / malformed). No gateway restart needed on success.
     */
    activate: (key: string) =>
      client.call('license.activate', { key }) as Promise<{
        success: boolean;
        status: LicenseSnapshot;
      }>,
    /** Partner (NFR) redeem-code path — free activation, same hot-reload. */
    redeem: (code: string, email?: string) =>
      client.call('license.redeem', {
        code,
        ...(email ? { email } : {}),
      }) as Promise<{ success: boolean; status: LicenseSnapshot }>,
  },
  marketplace: {
    list: () =>
      client.call('marketplace.list') as Promise<{ servers: MarketplaceServer[] }>,
    install: (id: string, agentId: string) =>
      client.call('marketplace.install', { id, agent_id: agentId }) as Promise<{ success: boolean; agent_id: string }>,
  },
  odoo: {
    status: () =>
      client.call('odoo.status') as Promise<OdooStatus>,
    config: () =>
      client.call('odoo.config') as Promise<OdooConfig | null>,
    configure: (config: OdooConfigUpdate) =>
      client.call('odoo.configure', { ...config }) as Promise<{ success: boolean }>,
    test: (params?: OdooTestParams) =>
      client.call('odoo.test', params ? { ...params } : {}) as Promise<{
        success: boolean;
        message: string;
      }>,
    /** Per-agent credential override. `configured:false` = inherits global. */
    agentConfigGet: (agentId: string) =>
      client.call('odoo.agent_config_get', { agent_id: agentId }) as Promise<OdooAgentConfig>,
    /** api_key/password write-only: new value sets, '' clears, omit keeps. */
    agentConfigSet: (params: OdooAgentConfigSet) =>
      client.call('odoo.agent_config_set', { ...params }) as Promise<OdooAgentConfigSetResult>,
    agentTest: (agentId: string) =>
      client.call('odoo.agent_test', { agent_id: agentId }) as Promise<{
        success: boolean;
        message: string;
      }>,
    /** Introspect models/fields, write a schema summary to the wiki, and
     *  return a lightweight model list. Metadata only — no business rows. */
    discoverSchema: (maxModels?: number) =>
      client.call(
        'odoo.discover_schema',
        maxModels ? { max_models: maxModels } : {},
      ) as Promise<OdooDiscoverSchemaResult>,
  },
  identity: {
    configGet: () =>
      client.call('identity.config_get') as Promise<IdentityConfig>,
    configSet: (config: IdentityConfigUpdate) =>
      client.call('identity.config_set', { ...config }) as Promise<{
        success: boolean;
        changes: string[];
      }>,
    resolve: (identifier: string, channel?: string) =>
      client.call('identity.resolve', {
        identifier,
        ...(channel ? { channel } : {}),
      }) as Promise<IdentityResolveResult>,
  },
  /**
   * The two Google credential paths that need no per-customer OAuth client:
   * service-account domain-wide delegation and the Apps Script bridge. The
   * OAuth path keeps its own connect flow under `mcp.oauth.*`.
   *
   * `get` never returns the stored bridge secret — only whether one is set.
   */
  googleCredentials: {
    get: () => client.call('google.credentials.get') as Promise<GoogleCredentialsStatus>,
    set: (params: GoogleCredentialsInput) =>
      client.call('google.credentials.set', params) as Promise<{ ok: boolean; mode: string }>,
    test: () =>
      client.call('google.credentials.test') as Promise<{
        ok: boolean;
        path: string;
        account?: string;
        detail: string;
      }>,
  },
  mcp: {
    list: () =>
      client.call('mcp.list') as Promise<{ agents: McpAgentConfig[]; catalog: McpCatalogItem[] }>,
    update: (agentId: string, action: 'add' | 'remove', serverName: string, serverDef?: McpServerDef) =>
      client.call('mcp.update', {
        agent_id: agentId,
        action,
        server_name: serverName,
        ...(serverDef ? { server_def: serverDef } : {}),
      }) as Promise<{ success: boolean }>,
    oauthProviders: () =>
      client.call('mcp.oauth.providers') as Promise<{ providers: McpOAuthProvider[] }>,
    oauthStart: (providerId: string, clientId?: string, clientSecret?: string) =>
      client.call('mcp.oauth.start', {
        provider_id: providerId,
        ...(clientId ? { client_id: clientId } : {}),
        ...(clientSecret ? { client_secret: clientSecret } : {}),
      }) as Promise<{ auth_url: string; state: string }>,
    oauthStatus: (providerId: string) =>
      client.call('mcp.oauth.status', { provider_id: providerId }) as Promise<{
        /** Connection state — true while a refresh token can renew the access token. */
        authenticated: boolean;
        /** Whether the access token itself is still fresh (it lasts ~1h on Google). */
        access_token_valid?: boolean;
        can_refresh?: boolean;
        expires_at: string | null;
        scopes?: string[];
      }>,
    oauthRevoke: (providerId: string) =>
      client.call('mcp.oauth.revoke', { provider_id: providerId }) as Promise<{ success: boolean }>,
    importFetch: (url: string) =>
      client.call('mcp.import.fetch', { url }) as Promise<McpImportFetchResult>,
    importInstall: (params: {
      agent_id: string;
      server_name: string;
      server_def: McpServerDef;
      add_to_catalog?: boolean;
      description?: string;
      source_url?: string;
    }) =>
      client.call('mcp.import.install', params) as Promise<{
        success: boolean;
        agent_id: string;
        server_name: string;
        catalog_added: boolean;
        warning?: string;
      }>,
    /** Non-admin: file an MCP install request for the manager→admin chain. */
    installRequest: (params: {
      agent_id: string;
      server_name: string;
      server_def: McpServerDef;
      add_to_catalog?: boolean;
      description?: string;
      source_url?: string;
    }) =>
      client.call('mcp.install_request', params) as Promise<InstallRequestFiled>,
  },
  installRequests: {
    /** Manager+: requests the caller can currently act on. */
    list: () =>
      client.call('install_requests.list') as Promise<{ requests: InstallRequestInfo[]; count: number }>,
    /** Any user: the caller's own requests + status. */
    mine: () =>
      client.call('install_requests.mine') as Promise<{ requests: InstallRequestInfo[]; count: number }>,
    /** Manager+: approve/deny; executes install on final approval. */
    decide: (id: string, approve: boolean, reason?: string) =>
      client.call('install_requests.decide', { id, approve, ...(reason ? { reason } : {}) }) as Promise<{
        status: string;
        stage?: string;
        executed?: boolean;
        detail?: unknown;
        warning?: string;
      }>,
  },
  tasks: {
    list: (filters?: {
      status?: TaskStatus;
      agent_id?: string;
      priority?: TaskPriority;
      goal_mode?: boolean;
    }) => client.call('tasks.list', filters ?? {}) as Promise<{ tasks: TaskInfo[] }>,
    create: (params: TaskCreateParams) =>
      client.call('tasks.create', { ...params }) as Promise<{ task: TaskInfo }>,
    update: (taskId: string, fields: TaskUpdateParams) =>
      client.call('tasks.update', { task_id: taskId, ...fields }) as Promise<{ task: TaskInfo }>,
    remove: (taskId: string) =>
      client.call('tasks.remove', { task_id: taskId }) as Promise<{ success: boolean }>,
    assign: (taskId: string, agentId: string) =>
      client.call('tasks.assign', { task_id: taskId, agent_id: agentId }) as Promise<{ task: TaskInfo }>,
    // L2: post a comment on a task; author is the authenticated caller.
    comment: (taskId: string, body: string) =>
      client.call('tasks.comment', { task_id: taskId, body }) as Promise<{ comment: TaskComment }>,
    // L2: list a task's comments (oldest first).
    comments: (taskId: string) =>
      client.call('tasks.comments', { task_id: taskId }) as Promise<{ comments: TaskComment[] }>,
    // Iterative Kanban: a task's revision timeline (dispatched → submitted →
    // verdict per round).
    iterations: (taskId: string) =>
      client.call('tasks.iterations', { task_id: taskId }) as Promise<{ iterations: TaskIteration[] }>,
    // WP-F (P2-c): file-change evidence for the needs_human 「變更」tab —
    // what the task's rounds actually wrote/edited/deleted, newest first.
    changes: (taskId: string, limit?: number) =>
      client.call('tasks.changes', limit ? { task_id: taskId, limit } : { task_id: taskId }) as Promise<TaskChanges>,
    // I-2b: the deliverables a task produced — the 「產物」tab. Read-only.
    artifacts: (taskId: string, limit?: number) =>
      client.call('tasks.artifacts', limit ? { task_id: taskId, limit } : { task_id: taskId }) as Promise<TaskArtifacts>,
    // Iterative Kanban: per-agent + board flow metrics. Non-admins pass an
    // agent_id and see only that agent's slice.
    flowMetrics: (agentId?: string) =>
      client.call('tasks.flow_metrics', agentId ? { agent_id: agentId } : {}) as Promise<FlowMetrics>,
    // ── Goal-loop management (/goals page, 2026-08-14) ────────
    /** Assign an autonomous goal from the dashboard — same semantics as the
     *  channel `/goal` command (goal_mode task, judge loop, needs_human
     *  escalation). */
    goalCreate: (params: {
      agent_id: string;
      description: string;
      acceptance_criteria?: string;
      outcome?: string;
      priority?: TaskPriority;
      /** Goal assignment form v2: optional wall-clock budget in hours
       *  (1-720). Omitted / undefined ⇒ the global `[goal_loop]
       *  wall_clock_hours` default applies. */
      duration_hours?: number;
      /** Goal assignment form v2: optional per-goal risk boundary text
       *  (≤2000 chars). Empty / undefined ⇒ the deployment baseline
       *  boundary (config `[goal_defaults] baseline_boundary`) applies. */
      risk_boundary?: string;
      /** Belief loop × goal contract gap 3
       *  (design-market-belief-loop-2026-08.md §3): opt into teaching the
       *  assigned agent to declare structured predictions (`belief_submit` /
       *  `belief_settle`) about the goal's measurable external indicators,
       *  folded into the judge's acceptance bar. Default `false`. */
      require_beliefs?: boolean;
      /** I-1c "想一想" (AssignSheet's third execution mode): generate a
       *  short human-readable plan and park the task `needs_human` for
       *  approval instead of letting the goal loop execute immediately.
       *  Default `false` (the existing `直接做` behaviour). */
      plan_first?: boolean;
    }) =>
      client.call('tasks.goal_create', { ...params }) as Promise<{
        task: TaskInfo;
        iteration_cap: number;
        dispatch_enabled: boolean;
        /** Echoes the request's `plan_first` — lets the caller show "計畫已
         *  生成，等待你核准" instead of the normal assign toast. */
        plan_first: boolean;
      }>,
    /** One goal task's whole loop story: rounds, task-scoped activity,
     *  pending kickoff approval. */
    timeline: (taskId: string) =>
      client.call('tasks.timeline', { task_id: taskId }) as Promise<GoalTimeline>,
    /** needs_human intervention through the SAME path as channel buttons
     *  (fail-closed, clears stale claim/lease on retry, collapses cards). */
    goalDecide: (taskId: string, action: 'retry' | 'done' | 'abort' | 'takeover', note?: string) =>
      client.call('tasks.goal_decide', { task_id: taskId, action, note: note ?? '' }) as Promise<{
        ok: boolean;
        message: string;
        task: TaskInfo | null;
      }>,
    /** I-3a "接著做": reopen a `done`/`failed`/`cancelled` goal task with a
     *  required follow-up message (new round, same acceptance criteria and
     *  risk boundary — still goes through the MAV judge). Separate from
     *  `goalDecide` because it acts on terminal statuses, not `needs_human`. */
    goalContinue: (taskId: string, message: string) =>
      client.call('tasks.goal_decide', { task_id: taskId, action: 'continue', message }) as Promise<{
        ok: boolean;
        message: string;
        task: TaskInfo | null;
      }>,
    // ── I-3b: task list operations (search/pin/archive/rename, 2026-08-15) ──
    // Thin wrappers over `tasks.archive`/`tasks.unarchive`/`tasks.pin`/
    // `tasks.unpin`/`tasks.rename` — the gateway implements each as a
    // convenience RPC delegating to `tasks.update` (same HS4 authorization,
    // same `{ task: TaskInfo }` response shape).
    archive: (taskId: string) =>
      client.call('tasks.archive', { task_id: taskId }) as Promise<{ task: TaskInfo }>,
    unarchive: (taskId: string) =>
      client.call('tasks.unarchive', { task_id: taskId }) as Promise<{ task: TaskInfo }>,
    pin: (taskId: string) =>
      client.call('tasks.pin', { task_id: taskId }) as Promise<{ task: TaskInfo }>,
    unpin: (taskId: string) =>
      client.call('tasks.unpin', { task_id: taskId }) as Promise<{ task: TaskInfo }>,
    rename: (taskId: string, title: string) =>
      client.call('tasks.rename', { task_id: taskId, title }) as Promise<{ task: TaskInfo }>,
    /** Paginated task listing with a total count — used by `/goals`'s 已結束／
     *  已封存 section to replace the old client-side `.slice(0, 20)` hard cut
     *  with real "load more" pagination. `archived` defaults to excluding
     *  archived rows server-side (see `list_tasks_paginated`), matching
     *  `tasks.list`'s existing default-hidden behaviour. */
    listPage: (params: {
      status?: TaskStatus;
      agent_id?: string;
      priority?: TaskPriority;
      goal_mode?: boolean;
      archived?: boolean;
      limit?: number;
      offset?: number;
    }) =>
      client.call('tasks.list_page', params) as Promise<{
        tasks: TaskInfo[];
        total: number;
        limit: number;
        offset: number;
      }>,
  },
  // U4 co-edited plans — a shared, ordered step list per AI employee that both
  // the user (here) and the agent (plan_get / plan_update_step MCP) edit.
  plans: {
    list: (filters?: { agent_id?: string; status?: PlanStatus }) =>
      client.call('plans.list', filters ?? {}) as Promise<{ plans: PlanInfo[] }>,
    get: (planId: string) =>
      client.call('plans.get', { plan_id: planId }) as Promise<{ plan: PlanInfo; steps: PlanStep[] }>,
    create: (params: PlanCreateParams) =>
      client.call('plans.create', { ...params }) as Promise<{ plan: PlanInfo; steps: PlanStep[] }>,
    update: (planId: string, fields: { title?: string; description?: string; status?: PlanStatus }) =>
      client.call('plans.update', { plan_id: planId, ...fields }) as Promise<{ plan: PlanInfo }>,
    remove: (planId: string) =>
      client.call('plans.remove', { plan_id: planId }) as Promise<{ success: boolean }>,
    addStep: (
      planId: string,
      params: { text: string; assignee_kind?: PlanAssigneeKind; assignee?: string; position?: number },
    ) => client.call('plans.add_step', { plan_id: planId, ...params }) as Promise<{ step: PlanStep }>,
    updateStep: (stepId: string, fields: PlanStepUpdateParams) =>
      client.call('plans.update_step', { step_id: stepId, ...fields }) as Promise<{ step: PlanStep }>,
    removeStep: (stepId: string) =>
      client.call('plans.remove_step', { step_id: stepId }) as Promise<{ success: boolean }>,
  },
  // RFC-24 Decision Continuity — an agent's still-open proposals awaiting a choice.
  decisions: {
    list: (agentId: string, limit?: number) =>
      client.call('decisions.list', { agent_id: agentId, limit }) as Promise<{
        decisions: DecisionInfo[];
      }>,
    dismiss: (agentId: string, decisionId: string) =>
      client.call('decisions.dismiss', {
        agent_id: agentId,
        decision_id: decisionId,
      }) as Promise<{ dismissed: boolean; decision_id: string }>,
  },
  activity: {
    list: (params?: { agent_id?: string; type?: ActivityType; limit?: number; offset?: number }) =>
      client.call('activity.list', params ?? {}) as Promise<{ events: ActivityEvent[]; total: number }>,
    subscribe: () =>
      client.call('activity.subscribe'),
    // No unsubscribe: the backend broadcasts activity events to every
    // authenticated WS client, so disconnecting the WS is the only way to
    // stop receiving them.
  },
  // G11 Work Timeline — company Gantt rows (tasks + activity + heartbeats).
  timeline: {
    list: (params?: { from?: string; to?: string; agent_id?: string }) =>
      client.call('timeline.list', params ?? {}) as Promise<TimelineListResult>,
  },
  // G12 Run inspector — per-run transcripts derived from session turns +
  // the MCP tool audit trail. Shapes live in lib/run-transcript.ts.
  runs: {
    list: (params?: { agent_id?: string; limit?: number }) =>
      client.call('runs.list', params ?? {}) as Promise<{ runs: RunSummary[] }>,
    get: (runId: string) =>
      client.call('runs.get', { run_id: runId }) as Promise<RunDetail>,
  },
  // G15 Live Canvas — agent-pushed HTML workspace. The HTML is sanitized
  // server-side at write time and MUST still be rendered only inside a fully
  // sandboxed iframe (`sandbox=""`, srcdoc) — see lib/canvas-doc.ts.
  canvas: {
    get: (agentId: string, seq?: number) =>
      client.call(
        'canvas.get',
        seq === undefined ? { agent_id: agentId } : { agent_id: agentId, seq },
      ) as Promise<CanvasGetResult>,
  },
  autopilot: {
    list: () =>
      client.call('autopilot.list') as Promise<{ rules: AutopilotRule[] }>,
    create: (params: AutopilotCreateParams) =>
      client.call('autopilot.create', { ...params }) as Promise<{ rule: AutopilotRule }>,
    update: (ruleId: string, fields: Partial<AutopilotCreateParams> & { enabled?: boolean }) =>
      client.call('autopilot.update', { rule_id: ruleId, ...fields }) as Promise<{ rule: AutopilotRule }>,
    remove: (ruleId: string) =>
      client.call('autopilot.remove', { rule_id: ruleId }) as Promise<{ success: boolean }>,
    history: (ruleId?: string, limit = 20) =>
      client.call('autopilot.history', { rule_id: ruleId, limit }) as Promise<{ entries: AutopilotHistoryEntry[] }>,
  },
  // Resident sensing observability (WP4) — read-only status for the
  // `[[tick.sources]]` config-driven ingest layer + its WP3 local screening.
  ticks: {
    sources: () =>
      client.call('ticks.sources') as Promise<TicksSourcesResult>,
    recent: (source: string, limit = 50) =>
      client.call('ticks.recent', { source, limit }) as Promise<TicksRecentResult>,
  },
  // RFC-26 Live Run Forking — read fork state from the cross-process ForkStore.
  fork: {
    list: (limit = 50) =>
      client.call('fork.list', { limit }) as Promise<{ forks: ForkSummary[] }>,
    inspect: (forkId: string) =>
      client.call('fork.inspect', { fork_id: forkId }) as Promise<ForkDetail>,
    resolve: (forkId: string, branchId: string) =>
      client.call('fork.resolve', { fork_id: forkId, branch_id: branchId }) as Promise<{
        fork_id: string;
        resolved: boolean;
        winner: string;
      }>,
  },
  sharedSkills: {
    list: () =>
      client.call('skills.shared') as Promise<{ skills: SharedSkillInfo[] }>,
    share: (agentId: string, skillName: string) =>
      client.call('skills.share', { agent_id: agentId, skill_name: skillName }) as Promise<{ success: boolean }>,
    adopt: (skillName: string, targetAgentId: string) =>
      client.call('skills.adopt', { skill_name: skillName, target_agent_id: targetAgentId }) as Promise<{ success: boolean }>,
  },
  partner: {
    profile: () =>
      client.call('partner.profile') as Promise<PartnerProfile>,
    stats: () =>
      client.call('partner.stats') as Promise<PartnerStats>,
    customers: (status?: string, limit = 100) =>
      client.call('partner.customers', { status, limit }) as Promise<{
        customers: PartnerCustomer[];
      }>,
    updateProfile: (
      input: Omit<PartnerProfile, 'created_at' | 'updated_at'>,
    ) =>
      client.call('partner.profile.update', input) as Promise<PartnerProfile>,
    addCustomer: (input: Omit<PartnerCustomer, 'id' | 'created_at'>) =>
      client.call('partner.customer.add', input) as Promise<{ id: string }>,
    updateCustomer: (
      id: string,
      patch: Partial<Omit<PartnerCustomer, 'id' | 'created_at'>>,
    ) =>
      client.call('partner.customer.update', { id, patch }) as Promise<{
        success: boolean;
      }>,
    deleteCustomer: (id: string) =>
      client.call('partner.customer.delete', { id }) as Promise<{
        success: boolean;
      }>,
    // License generation is CLI-only (`duduclaw license generate`); the dashboard
    // intentionally exposes no RPC for it (UI.4).
  },
  contract: {
    get: (agentId: string) =>
      client.call('contract.get', { agent_id: agentId }) as Promise<
        ContractConfig & { agent_id: string }
      >,
    update: (agentId: string, fields: ContractConfig) =>
      client.call('contract.update', { agent_id: agentId, ...fields }) as Promise<
        ContractConfig & { success: boolean; agent_id: string; message: string }
      >,
  },
  redaction: {
    get: () => client.call('redaction.get') as Promise<RedactionConfig>,
    update: (fields: RedactionUpdate) =>
      client.call('redaction.update', { ...fields }) as Promise<{
        success: boolean;
        changes: string[];
        /** True when the live pipeline was hot-reloaded with the new config. */
        applied: boolean;
        /** Set when the config was saved but could not be applied live. */
        warning: string | null;
      }>,
    stats: () => client.call('redaction.stats') as Promise<RedactionStats>,
    recentAudit: (limit = 50) =>
      client.call('redaction.recent_audit', { limit }) as Promise<{
        entries: RedactionAuditEntry[];
      }>,
    policyStatus: () =>
      client.call('redaction.policy_status') as Promise<RedactionPolicyStatus>,
    overrideStatus: () =>
      client.call('redaction.override_status') as Promise<RedactionOverrideStatus>,
  },
  skillSynthesis: {
    get: () => client.call('skill_synthesis.get') as Promise<SkillSynthesisConfig>,
    update: (fields: SkillSynthesisUpdate) =>
      client.call('skill_synthesis.update', { ...fields }) as Promise<{
        success: boolean;
        changes: string[];
      }>,
  },
  mcpKeys: {
    list: () => client.call('mcp_keys.list') as Promise<{ keys: McpKeyEntry[] }>,
    create: (params: { client_id: string; is_external: boolean; scopes: string[]; env?: 'prod' | 'staging' | 'dev' }) =>
      client.call('mcp_keys.create', { ...params }) as Promise<McpKeyCreateResult>,
    revoke: (key: string) =>
      client.call('mcp_keys.revoke', { key }) as Promise<{ success: boolean; revoked: string }>,
  },
  killswitch: {
    get: () => client.call('killswitch.get') as Promise<KillswitchConfig>,
    update: (fields: KillswitchUpdate) =>
      client.call('killswitch.update', { ...fields }) as Promise<{
        success: boolean;
        changes: string[];
        message: string;
      }>,
  },
  governance: {
    /** List policies. Omit agent_id for global + every per-agent file. */
    list: (agentId?: string) =>
      client.call('governance.list', agentId ? { agent_id: agentId } : {}) as Promise<{
        policies: GovPolicy[];
      }>,
    /** Create or replace a policy (matched by policy_id within its scope). */
    upsert: (policy: GovPolicy) =>
      client.call('governance.upsert', { ...policy }) as Promise<{
        success: boolean;
        scope: string;
        policy_id: string;
        created: boolean;
        message: string;
      }>,
    remove: (policyId: string, agentId?: string) =>
      client.call('governance.remove', {
        policy_id: policyId,
        ...(agentId ? { agent_id: agentId } : {}),
      }) as Promise<{ success: boolean; removed: string }>,
  },
  wikiScope: {
    get: () =>
      client.call('wiki_scope.get') as Promise<{ namespaces: WikiScopeNamespace[] }>,
    /** Set (or, with remove=true, clear → agent_writable default) a namespace policy. */
    update: (params: {
      namespace: string;
      mode?: WikiScopeMode;
      synced_from?: string;
      remove?: boolean;
    }) =>
      client.call('wiki_scope.update', params) as Promise<{ success: boolean; change: string }>,
  },
  inference: {
    /** Read the full inference.toml. The openai_compat api_key is masked
     *  (`api_key_set` + a placeholder); treat it as write-only. */
    get: () => client.call('inference.get') as Promise<InferenceConfig>,
    /** Partial update. Omit a section to leave it untouched. For openai_compat,
     *  omit `api_key` to keep the stored secret; '' clears it. */
    update: (fields: InferenceUpdate) =>
      client.call('inference.update', { ...fields }) as Promise<{
        success: boolean;
        changes: string[];
      }>,
  },
  migrate: {
    /** Dry-run preview — reads the source platform and reports what WOULD be
     *  imported / skipped / conflicted. Writes nothing. manager-gated. */
    scan: (platform: MigratePlatform, source?: string) =>
      client.call('migrate.scan', migrateScanArgs(platform, source)) as Promise<MigrateResult>,
    /** Execute the migration — writes agents/tokens/skills/etc. and returns the
     *  same shape with `dry_run:false` + a `report_path`. May run up to 300s
     *  server-side, so a 300s response timeout is used. manager-gated. */
    apply: (platform: MigratePlatform, source?: string, rename?: boolean) =>
      client.call(
        'migrate.apply',
        migrateApplyArgs(platform, source, rename),
        false,
        300000,
      ) as Promise<MigrateResult>,
  },
  users: {
    list: () =>
      client.call('users.list') as Promise<{ users: UserDetail[] }>,
    /** Active users strictly below the caller's rank (manager+). Minimal
     *  fields — feeds the read-only dashboard viewer picker. */
    subordinates: () =>
      client.call('users.subordinates') as Promise<{
        users: Array<{ id: string; display_name: string; role: string }>;
      }>,
    create: (params: { email: string; display_name: string; password: string; role?: string; department?: string }) =>
      client.call('users.create', params) as Promise<{ user: UserInfo }>,
    /** `department: ''` clears the user's department; omit to leave unchanged. */
    update: (params: { user_id: string; display_name?: string; role?: string; password?: string; department?: string }) =>
      client.call('users.update', params) as Promise<{ status: string }>,
    remove: (userId: string) =>
      client.call('users.remove', { user_id: userId }) as Promise<{ status: string }>,
    bindAgent: (userId: string, agentName: string, accessLevel?: string) =>
      client.call('users.bind_agent', {
        user_id: userId,
        agent_name: agentName,
        access_level: accessLevel ?? 'owner',
      }) as Promise<{ status: string }>,
    unbindAgent: (userId: string, agentName: string) =>
      client.call('users.unbind_agent', { user_id: userId, agent_name: agentName }) as Promise<{ status: string }>,
    offboard: (userId: string, transferTo?: string) =>
      client.call('users.offboard', { user_id: userId, transfer_to: transferTo }) as Promise<{
        status: string;
        transferred_agents: string[];
      }>,
    me: () =>
      client.call('users.me') as Promise<{ user: UserInfo; bindings: UserAgentBinding[] }>,
    // Self-service: the logged-in user changes their OWN password (works in the
    // single-owner edition where the Users page is hidden).
    changePassword: (currentPassword: string, newPassword: string) =>
      client.call('users.change_password', {
        current_password: currentPassword,
        new_password: newPassword,
      }) as Promise<{ status: string }>,
    auditLog: (params?: { user_id?: string; action?: string; limit?: number }) =>
      client.call('users.audit_log', params ?? {}) as Promise<{ entries: AuditEntry[] }>,
  },
  // ── Branding / white-label ────────────────────────────────────
  branding: {
    /** Read the active branding + fixed vendor block. Any authed user. */
    get: () => client.call('branding.get') as Promise<BrandingGetResponse>,
    /** Update the distributor branding. admin + white_label gated (fail-closed). */
    set: (input: BrandingSetInput) =>
      client.call('branding.set', { ...input }) as Promise<{ ok: boolean; branding: BrandingConfig }>,
    /** Clear all custom branding → revert to DuDuClaw defaults. */
    reset: () => client.call('branding.reset') as Promise<{ ok: boolean }>,
    /** Sanitize raw About-page HTML for live preview. admin + white_label gated. */
    preview: (aboutHtml: string) =>
      client.call('branding.preview', { about_html: aboutHtml }) as Promise<{
        ok: boolean;
        sanitized_html: string;
      }>,
    bundle: {
      /** Produce a signed branding bundle for this instance to ship to customers. */
      create: () =>
        client.call('branding.bundle.create') as Promise<{
          ok: boolean;
          bundle: BrandingBundle;
        }>,
    },
  },
  about: {
    /** About-page payload: distributor branding + fixed vendor + version/tier. */
    get: () => client.call('about.get') as Promise<AboutResponse>,
  },
  // ── Distributor management (owner instance only) ──────────────
  distributor: {
    /** Whether an issuer key is configured + aggregate stats. */
    status: () =>
      client.call('distributor.status') as Promise<{
        issuer_configured: boolean;
        issuer_key_id: string;
        /** P2: whether the /v1/license/refresh + /crl control-plane is live. */
        refresh_endpoint_active?: boolean;
        stats: DistributorStats;
      }>,
    list: () =>
      client.call('distributor.list') as Promise<{ distributors: DistributorProfile[] }>,
    add: (input: DistributorInput) =>
      client.call('distributor.add', { ...input }) as Promise<{ ok: boolean; distributor: DistributorProfile }>,
    update: (id: string, patch: DistributorPatch) =>
      client.call('distributor.update', { id, patch }) as Promise<{ ok: boolean }>,
    remove: (id: string) =>
      client.call('distributor.remove', { id }) as Promise<{ ok: boolean }>,
    /** Sign a new OEM white-label license for the distributor's machine. */
    issue: (params: {
      distributor_id: string;
      machine_fingerprint: string;
      expires_days?: number;
      note?: string;
      /** WP8: narrow the customer's editable branding range (serde field names).
       *  Omit ⇒ full reseller (Vendor) range. Provide ⇒ Customer scope limited to
       *  these fields (non-branding names are rejected by the gateway). */
      branding_editable?: string[];
    }) =>
      client.call('distributor.issue', { ...params }) as Promise<{
        ok: boolean;
        license_blob: string;
        record: IssuedLicense;
      }>,
    /** Locally mark a license revoked. Remote propagation needs a CRL publish. */
    revoke: (licenseId: string) =>
      client.call('distributor.revoke', { license_id: licenseId }) as Promise<{
        ok: boolean;
        crl_note: string;
      }>,
    bundle: {
      /** Owner-side counter-sign of a branding bundle for a distributor (used
       *  when the distributor instance cannot reach this gateway directly). */
      sign: (params: { distributor_id: string; branding: BrandingConfig }) =>
        client.call('distributor.bundle.sign', { ...params }) as Promise<{
          ok: boolean;
          bundle: BrandingBundle;
        }>,
    },
  },
  // ── OS-native page (P4-3) ───────────────────────────────────────
  os: {
    /** Whole-fleet OS-native snapshot: edition, quota, per-agent status. */
    status: () => client.call('os.status') as Promise<OsStatusResult>,
    /** Per-agent OS-native settings write. Rejects with a structured
     *  `{ code: OS_NATIVE_QUOTA_ERROR_CODE, message }` error when the write
     *  would exceed the edition's OS-native seat quota. */
    settingsUpdate: (params: OsSettingsUpdateParams) =>
      client.call('os.settings.update', { ...params }) as Promise<OsSettingsUpdateResult>,
    /** Proactivity gate decision tail + four-quadrant outcome tally.
     *  `n` defaults to 50, clamped 1-200 server-side. */
    gateRecent: (params?: { n?: number; agent_id?: string }) =>
      client.call('os.gate.recent', params ?? {}) as Promise<OsGateRecentResult>,
    /** Most recent `os_*` perception events, newest first. `n` defaults to
     *  50, clamped 1-200 server-side. */
    eventsRecent: (params?: { n?: number }) =>
      client.call('os.events.recent', params ?? {}) as Promise<OsEventsRecentResult>,
    /** Opt this WebSocket connection into a live `os.events.entry` tail of
     *  os_file/os_frontmost events. Admin-gated, same as every other `os.*`
     *  RPC. Pair with `client.subscribe('os.events.entry', ...)`. */
    eventsSubscribe: () =>
      client.call('os.events.subscribe') as Promise<{ success: true; subscribed: true }>,
    /** Counterpart to `eventsSubscribe` — stops the live push for this
     *  connection. */
    eventsUnsubscribe: () =>
      client.call('os.events.unsubscribe') as Promise<{ success: true; subscribed: false }>,
    /** On-demand OS-native environment probes — the only expensive OS RPC
     *  (live notification / frontmost / calendar / mdfind checks). */
    doctorRun: () => client.call('os.doctor.run') as Promise<OsDoctorRunResult>,
  },
  // ── I-5: ⌘K cross-source content search ─────────────────────────
  search: {
    /** One bounded query fanned out over conversations / artifacts / memory /
     *  knowledge (agent + shared wiki). Non-admin callers must pass
     *  `agent_id` (memory and agent-local wiki always require one — see the
     *  gateway doc comment on `handle_search_query`). `sources` narrows which
     *  surfaces are queried; omitted ⇒ every source. */
    query: (params: { q: string; agent_id?: string; limit?: number; sources?: string[] }) =>
      client.call('search.query', params) as Promise<{
        query: string;
        hits: SearchHit[];
        truncated: boolean;
      }>,
  },
  // ── Agent preset P1 (read-only dashboard card) ───────────────────
  presets: {
    /** Every preset under `~/.duduclaw/presets/`, read-only. A preset that
     *  failed to parse still appears with `.error` set. */
    list: () => client.call('presets.list') as Promise<{ presets: PresetSummary[] }>,
    /** One agent's current preset binding + live resolution outcome +
     *  which of its own `agent.toml` fields override the preset. */
    status: (agentId: string) =>
      client.call('presets.status', { agent_id: agentId }) as Promise<{
        agent_id: string;
        resolution: PresetResolution;
      }>,
  },
  // ── WP-C: appliance device management ("裝置" page) ────────────────
  // Every RPC is admin + appliance-only server-side (`require_admin!()` +
  // `require_appliance!()`); a non-appliance install rejects all of these
  // with the structured `not_appliance` error. `useIsAppliance` (R2,
  // 2026-08) no longer probes this admin-gated surface — it reads the
  // non-admin `is_appliance` field on `system.status` instead, so a
  // manager/employee viewer can also learn whether the box is an appliance.
  device: {
    status: () => client.call('device.status') as Promise<DeviceStatus>,
    /** Read-only interface list. Setting a static IP is not implemented yet
     *  (the gateway refuses any network-write-shaped param with
     *  `not_implemented` — this client never sends one). */
    network: () =>
      client.call('device.network') as Promise<{ interfaces: DeviceNetworkInterface[] }>,
    /** `systemd-sysupdate list --json=short`, forwarded verbatim in `.stdout`. */
    updateStatus: () => client.call('device.update_status') as Promise<DeviceOpResult>,
    /** Installs the newest available update. */
    updateApply: () => client.call('device.update_apply') as Promise<DeviceOpResult>,
    /** Always rejects `unsupported` this round (no verified appliance A/B
     *  rollback mechanism yet — see the gateway doc comment) — kept for the
     *  seam, never called from the UI, which shows the button disabled. */
    updateRollback: () => client.call('device.update_rollback', { confirm: true }) as Promise<DeviceOpResult>,
    /** Archives the device's writable data partition. Feed `.filename` to
     *  `GET /api/files/download?name=<filename>` to fetch it. */
    backupCreate: () => client.call('device.backup_create') as Promise<DeviceBackupResult>,
    /** Wipes device state and re-provisions on next boot. Irreversible. */
    factoryReset: () =>
      client.call('device.factory_reset', { confirm: true }) as Promise<DeviceOpResult>,
    power: (action: 'restart' | 'shutdown') =>
      client.call('device.power', { action, confirm: true }) as Promise<DeviceOpResult>,
    // ── WP-G1: scheduled backups + device-migration restore ──────────
    backupScheduleGet: () =>
      client.call('device.backup_schedule_get') as Promise<DeviceBackupScheduleConfig>,
    backupScheduleSet: (patch: Partial<DeviceBackupScheduleConfig>) =>
      client.call('device.backup_schedule_set', patch) as Promise<DeviceBackupScheduleConfig>,
    /** Lists files under the dedicated `<home>/backups/` directory. */
    backupList: () => client.call('device.backup_list') as Promise<{ files: DeviceBackupFileEntry[] }>,
    backupDelete: (name: string) =>
      client.call('device.backup_delete', { name }) as Promise<{ deleted: true }>,
    /** Stage an already-uploaded (`POST /api/device/backup-upload`) archive
     *  for restore on next boot. Nothing destructive happens until the
     *  gateway actually restarts. */
    backupRestore: (path: string) =>
      client.call('device.backup_restore', { path, confirm: true }) as Promise<DeviceBackupRestoreResult>,
  },
};
