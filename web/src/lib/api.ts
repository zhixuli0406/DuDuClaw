import { client } from './ws-client';
import { migrateScanArgs, migrateApplyArgs } from './migrate';

// Type definitions matching Rust types
export interface AgentInfo {
  name: string;
  display_name: string;
  role: 'main' | 'specialist' | 'worker';
  status: 'active' | 'paused' | 'terminated';
  trigger: string;
  icon: string;
  reports_to: string;
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

export type TaskStatus = 'todo' | 'in_progress' | 'done' | 'blocked';
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
  assigned_to: string;
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
}

// ── User management types ────────────────────────────────────

export interface UserInfo {
  id: string;
  email: string;
  display_name: string;
  role: 'admin' | 'manager' | 'employee';
  status: 'active' | 'suspended' | 'offboarded';
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

export type RuntimeProvider = 'claude' | 'codex' | 'gemini' | 'antigravity' | 'openai_compat';

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
}

// ── CON: per-agent CONTRACT.toml ────────────────────────────────

export interface ContractConfig {
  must_not: string[];
  must_always: string[];
  max_tool_calls_per_turn: number;
}

// ── RED: global [redaction] ─────────────────────────────────────

export type RedactionSourceMode = 'on' | 'off' | 'selective' | 'inherit';

export interface RedactionSources {
  user_input: RedactionSourceMode;
  tool_results: RedactionSourceMode;
  system_prompt: RedactionSourceMode;
  sub_agent: RedactionSourceMode;
  cron_context: RedactionSourceMode;
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

// API namespace
export const api = {
  agents: {
    list: () =>
      client.call('agents.list') as Promise<{ agents: AgentDetail[] }>,
    status: (agentId: string) =>
      client.call('agents.status', { agent_id: agentId }) as Promise<AgentDetail>,
    create: (params: {
      name: string;
      display_name: string;
      role?: string;
      trigger?: string;
      soul?: string;
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
    update: (agentId: string, fields: AgentUpdateParams) =>
      client.call('agents.update', { agent_id: agentId, ...fields }) as Promise<{ success: boolean }>,
    remove: (agentId: string) =>
      client.call('agents.remove', { agent_id: agentId }) as Promise<{ success: boolean }>,
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
      }>,
    updateConfig: (fields: Record<string, unknown>) =>
      client.call('system.update_config', fields) as Promise<{ success: boolean; changes: string[] }>,
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
  billing: {
    usage: () =>
      client.call('billing.usage') as Promise<BillingUsage>,
    history: () =>
      client.call('billing.history') as Promise<{ invoices: BillingInvoice[] }>,
  },
  // WP14-T14.7 — HITL approval center. `list` is manager-gated; `decide` errors
  // on already-terminal requests or board-kind requests without admin scope.
  approvals: {
    list: (agentId?: string) =>
      client.call('approvals.list', agentId ? { agent_id: agentId } : {}) as Promise<{
        approvals: ApprovalItem[];
        count: number;
      }>,
    decide: (id: string, approve: boolean) =>
      client.call('approvals.decide', { id, approve }) as Promise<{
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
      }>,
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
    create: (params: { email: string; display_name: string; password: string; role?: string }) =>
      client.call('users.create', params) as Promise<{ user: UserInfo }>,
    update: (params: { user_id: string; display_name?: string; role?: string; password?: string }) =>
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
};
