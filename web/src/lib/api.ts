import { client } from './ws-client';

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

export interface SystemStatus {
  version: string;
  uptime_seconds: number;
  agents_count: number;
  channels_connected: number;
  gateway_address: string;
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

export interface SkillIndexEntry {
  name: string;
  description: string;
  tags: string[];
  author: string;
  url: string;
  compatible: string[];
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

export interface LicenseInfo {
  tier: string;
  activated: boolean;
  expires_at?: string;
  days_remaining?: number;
  features: string[];
  machine_fingerprint?: string;
  customer_name?: string;
  max_agents?: number;
  max_channels?: number;
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

export interface McpServerDef {
  command: string;
  args: string[];
  env: Record<string, string>;
}

export interface McpAgentConfig {
  agent_id: string;
  servers: Record<string, McpServerDef>;
}

export interface McpCatalogItem {
  id: string;
  name: string;
  description: string;
  category: string;
  requires_oauth: boolean;
  default_def: McpServerDef;
  required_env: string[];
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
  },
  evolution: {
    status: () =>
      client.call('evolution.status') as Promise<{
        enabled: boolean;
        mode: string;
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
      client.call('system.version') as Promise<{ version: string; auto_update: boolean; edition: string }>,
    config: () =>
      client.call('system.config') as Promise<Record<string, unknown>>,
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
        tasks: Array<{ id: string; agent_id: string; cron: string; enabled: boolean }>;
      }>,
    add: (agentId: string, cron: string, task: string) =>
      client.call('cron.add', { agent_id: agentId, cron, task }),
    pause: (id: string) =>
      client.call('cron.pause', { id }),
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
  skillMarket: {
    search: (query: string) =>
      client.call('skills.search', { query }) as Promise<{ skills: SkillIndexEntry[] }>,
  },
  models: {
    list: () =>
      client.call('models.list') as Promise<{
        models: Array<{ id: string; label: string; type: 'cloud' | 'local'; file?: string; size_bytes?: number }>;
        default_local: string | null;
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
  license: {
    status: () =>
      client.call('license.status') as Promise<LicenseInfo>,
    activate: (key: string) =>
      client.call('license.activate', { key }) as Promise<{ success: boolean }>,
    deactivate: () =>
      client.call('license.deactivate') as Promise<{ success: boolean }>,
  },
  marketplace: {
    install: (serverId: string) =>
      client.call('marketplace.install', { server_id: serverId }) as Promise<{ success: boolean }>,
  },
  odoo: {
    status: () =>
      client.call('odoo.status') as Promise<OdooStatus>,
    config: () =>
      client.call('odoo.config') as Promise<OdooConfig | null>,
    configure: (config: OdooConfigUpdate) =>
      client.call('odoo.configure', { ...config }) as Promise<{ success: boolean }>,
    test: () =>
      client.call('odoo.test') as Promise<{ success: boolean; message: string }>,
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
  },
  activity: {
    list: (params?: { agent_id?: string; type?: ActivityType; limit?: number; offset?: number }) =>
      client.call('activity.list', params ?? {}) as Promise<{ events: ActivityEvent[]; total: number }>,
    subscribe: () =>
      client.call('activity.subscribe'),
    unsubscribe: () =>
      client.call('activity.unsubscribe'),
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
  sharedSkills: {
    list: () =>
      client.call('skills.shared') as Promise<{ skills: SharedSkillInfo[] }>,
    share: (agentId: string, skillName: string) =>
      client.call('skills.share', { agent_id: agentId, skill_name: skillName }) as Promise<{ success: boolean }>,
    adopt: (skillName: string, targetAgentId: string) =>
      client.call('skills.adopt', { skill_name: skillName, target_agent_id: targetAgentId }) as Promise<{ success: boolean }>,
  },
  // Partner portal — backend not yet implemented; calls will reject with error
  partner: {
    generateLicense: (_params: { tier: string; customer: string; months: number }) =>
      Promise.reject(new Error('Partner license generation not yet available')) as Promise<{ key: string }>,
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
    auditLog: (params?: { user_id?: string; action?: string; limit?: number }) =>
      client.call('users.audit_log', params ?? {}) as Promise<{ entries: AuditEntry[] }>,
  },
};
