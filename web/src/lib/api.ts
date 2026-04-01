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
  license_key?: string;
  activated: boolean;
  expires_at?: string;
  days_remaining?: number;
  features: string[];
  fingerprint?: string;
  machine_fingerprint?: string;
  customer_name?: string;
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
    add: (type: string, config: Record<string, string>) =>
      client.call('channels.add', { type, config }),
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
  skills: {
    list: (agentId?: string) =>
      client.call('skills.list', { agent_id: agentId }) as Promise<{ skills: SkillInfo[] }>,
    content: (agentId: string, skillName: string) =>
      client.call('skills.content', {
        agent_id: agentId,
        skill_name: skillName,
      }) as Promise<{ content: string }>,
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
      client.call('system.version') as Promise<{ version: string }>,
    config: () =>
      client.call('system.config') as Promise<Record<string, unknown>>,
    updateConfig: (fields: { log_level?: string; rotation_strategy?: string }) =>
      client.call('system.update_config', fields) as Promise<{ success: boolean; changes: string[] }>,
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
  partner: {
    generateLicense: (params: { tier: string; customer: string; months: number }) =>
      client.call('partner.generate_license', params) as Promise<{ key: string }>,
  },
};
