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
  quiet_hours_start: number;
  quiet_hours_end: number;
  max_messages_per_hour: number;
  notify_channel: string;
  notify_chat_id: string;
}

export interface ChannelStatus {
  name: string;
  connected: boolean;
  last_connected?: string;
  error?: string;
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
  security_status?: 'pass' | 'warn' | 'fail';
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

export type TaskStatus = 'todo' | 'in_progress' | 'done' | 'blocked' | 'needs_human';
export type TaskPriority = 'low' | 'medium' | 'high' | 'urgent';

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
  parent_task_id?: string;
  tags: string[];
  message_id?: string;
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
  | 'skill_learned'
  | 'evolution_triggered'
  | 'autopilot_triggered'
  | 'autopilot_lag'
  | 'error';

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
  | 'schedule';

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

export interface EvolutionVersion {
  version_id: string;
  agent_id: string;
  soul_summary: string;
  soul_hash: string;
  applied_at: string;
  observation_end: string;
  status: string;
  pre_metrics: EvolutionMetrics;
  post_metrics: EvolutionMetrics | null;
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
  name: string;
  auth_url: string;
  scopes: string[];
  configured: boolean;
  token_status: 'none' | 'authenticated' | 'expired';
  expires_at: string | null;
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
  claude_oauth: boolean;
  claude_subscription: string | null;
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
  | 'admin';

/** All known MCP scopes — mirrors gateway `KNOWN_MCP_SCOPES`. */
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

export interface ApprovalItem {
  id: string;
  agent_id: string;
  kind: ApprovalKind;
  summary: string;
  /** Opaque request payload — shape varies by kind. */
  payload: unknown;
  created_at: string;
  ttl_seconds: number;
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

/** One past WebChat session, as returned by `chat.sessions.list`
 *  (newest first, archived excluded). */
export interface ChatSessionSummary {
  session_id: string;
  agent_id: string;
  /** First user message, CJK-safe 80-char truncation. May be empty. */
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
    }) =>
      client.call('agents.create', params) as Promise<{ success: boolean; agent: AgentInfo }>,
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
      client.call('agents.update', { agent_id: agentId, ...fields }) as Promise<{ success: boolean }>,
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
  },
  channels: {
    status: () =>
      client.call('channels.status') as Promise<{ channels: ChannelStatus[] }>,
    add: (type: string, config: Record<string, string>, agent?: string) =>
      client.call('channels.add', { type, config, ...(agent ? { agent } : {}) }),
    test: (type: string) =>
      client.call('channels.test', { type }) as Promise<{ success: boolean; message: string }>,
    remove: (type: string) =>
      client.call('channels.remove', { type }),
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
  },
  // Interactive CLI login ("Dashboard 一鍵登入") — drives a CLI's native login
  // in a PTY on the gateway and streams it back via `auth.cli_login.*` events.
  auth: {
    cliLoginStart: (runtime: 'claude' | 'codex' | 'gemini' | 'antigravity') =>
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
        reason?: string;
      }>,
  },

  accounts: {
    list: () =>
      client.call('accounts.list') as Promise<{ accounts: AccountInfo[] }>,
    budgetSummary: () =>
      client.call('accounts.budget_summary') as Promise<BudgetSummary>,
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
    search: (agentId: string, query: string, limit = 20) =>
      client.call('memory.search', {
        agent_id: agentId,
        query,
        limit,
      }) as Promise<{ entries: MemoryEntry[] }>,
    browse: (agentId: string, limit = 20) =>
      client.call('memory.browse', {
        agent_id: agentId,
        limit,
      }) as Promise<{ entries: MemoryEntry[] }>,
    keyFacts: (agentId: string, limit = 50) =>
      client.call('memory.key_facts', {
        agent_id: agentId,
        limit,
      }) as Promise<{ entries: KeyFactEntry[] }>,
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
  },
  wiki: {
    pages: (agentId: string) =>
      client.call('wiki.pages', { agent_id: agentId }) as Promise<{ pages: WikiPageMeta[]; exists: boolean }>,
    read: (agentId: string, pagePath: string) =>
      client.call('wiki.read', { agent_id: agentId, page_path: pagePath }) as Promise<{ content: string; path: string }>,
    search: (agentId: string, query: string, limit = 10) =>
      client.call('wiki.search', { agent_id: agentId, query, limit }) as Promise<{ hits: WikiSearchHit[] }>,
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
    list: (agentId?: string) =>
      client.call('skills.list', { agent_id: agentId }) as Promise<{ skills: SkillInfo[] }>,
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
  },
  system: {
    status: () =>
      client.call('system.status') as Promise<SystemStatus>,
    doctor: () =>
      client.call('system.doctor') as Promise<{
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
      }>,
    updateConfig: (fields: Record<string, unknown>) =>
      client.call('system.update_config', fields) as Promise<{ success: boolean; changes: string[]; applied?: boolean }>,
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
      }>,
    applyUpdate: () =>
      client.call('system.apply_update', {}) as Promise<{
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
  },
  skillMarket: {
    search: (query: string) =>
      client.call('skills.search', { query }) as Promise<{ skills: SkillIndexEntry[] }>,
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
        authenticated: boolean;
        expires_at: string | null;
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
    list: (filters?: { status?: TaskStatus; agent_id?: string; priority?: TaskPriority }) =>
      client.call('tasks.list', filters ?? {}) as Promise<{ tasks: TaskInfo[] }>,
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
};
