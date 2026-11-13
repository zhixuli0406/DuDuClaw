/**
 * Dashboard transport fence. Keep exemptions as EXACT method names here; do not
 * infer safety from a "get/list/status" substring. Every unlisted RPC (including
 * future methods) with an agent target is guarded. The gateway remains authoritative.
 *
 * API inventory: api.ts, api-custom-skills.ts, knowledge-api.ts, discovery-api.ts,
 * computer-session.ts and the HTTP task/causal/meeting clients. These are agent
 * references, not labels, counters, WeCom agent IDs, or free-form prompt text.
 */
export const AGENT_TARGET_KEYS = new Set([
  'agent_id', 'agentId', 'agent', 'agent_name', 'target_agent', 'target_agent_id',
  'assigned_to', 'assignee', 'from_agent', 'to_agent', 'agent_ids', 'reports_to',
  'owner_agent_id', 'lead_agent_name', 'audit_agent', 'verifier_agent', 'agent_override',
  'default_agent', 'primary_agent_id',
]);

// Never gate discovery of identity/grants, authentication or bootstrap itself.
export const AGENT_GUARD_BOOTSTRAP_METHODS = new Set([
  'connect', 'connect.challenge', 'ping', 'agents.callable',
  'users.me', 'users.change_password', 'system.status', 'system.product_profile',
  'auth.cli_login.start', 'auth.cli_login.status', 'auth.cli_login.input',
  'auth.cli_login.finalize', 'auth.cli_login.cancel',
]);

export const AGENT_GUARD_READ_ONLY_METHODS = new Set([
  'about.get', 'accounts.budget_summary', 'accounts.health',
  'accounts.list', 'activity.list', 'agent_files.diff',
  'agent_files.get', 'agent_files.history', 'agent_files.list',
  'agent_files.pending_get', 'agent_files.runtime_files', 'agents.avatar',
  'agents.channels.status', 'agents.export_profile', 'agents.google_identity_get',
  'agents.health', 'agents.inspect', 'agents.list',
  'agents.org_chart', 'agents.status', 'agents.tool_pruning_status',
  'analytics.conversations', 'analytics.cost_savings', 'analytics.summary',
  'approvals.list', 'assign_schedules.list', 'audit.evolution_query',
  'audit.reliability_summary', 'audit.unified_log', 'auth.cli_login.status',
  'autopilot.history', 'autopilot.list', 'belief.recent',
  'belief.summary', 'billing.history', 'billing.usage',
  'browser.audit_log', 'browser.browserbase_cost', 'browser.browserbase_sessions',
  'canvas.get', 'channels.access_get', 'channels.config_get',
  'channels.status', 'chat.sessions.history', 'chat.sessions.list',
  'computer_sessions.status', 'config.raw.get', 'contract.get',
  'cost.agents', 'cost.recent', 'cost.summary',
  'credentials.audit_get', 'credentials.audit_list', 'cron.list',
  'cron.templates', 'dashboard.layout.get', 'dashboard.layout.view',
  'dashboard.widgets.catalog', 'data_exports.get', 'db_sources.grants.list',
  'db_sources.list', 'db_sources.tables', 'decisions.list',
  'delegation.effective', 'delegation.get', 'departments.list', 'device.backup_list',
  'device.backup_schedule_get', 'device.network', 'device.status',
  'device.update_status', 'digest.latest', 'discovery.artifact',
  'discovery.catalog', 'discovery.list', 'discovery.tree',
  'distributor.list', 'distributor.status', 'evolution.stagnation',
  'evolution.status', 'evolution.telemetry', 'experts.catalog',
  'experts.list', 'finetune.datasets.list', 'finetune.datasets.preview',
  'finetune.jobs.list', 'finetune.jobs.status', 'fork.inspect',
  'fork.list', 'forward.calibration', 'forward.recent',
  'forward.states', 'forward.summary', 'gallery.list',
  'google.credentials.get', 'google.delegation.get', 'growth.daily_report',
  'growth.snapshot', 'heartbeat.status', 'identity.config_get',
  'inference.get', 'inference.local.catalog', 'inference.local.status',
  'install_requests.list', 'install_requests.mine', 'invitation.get',
  'invitation.preview', 'killswitch.get', 'knowledge.access_log.denied',
  'knowledge.access_log.list', 'knowledge.acl.get', 'knowledge.agent.collections',
  'knowledge.agent.data_sources', 'knowledge.citations.for_session', 'knowledge.collections.get',
  'knowledge.collections.list', 'knowledge.embedding.status', 'knowledge.limits',
  'knowledge.passage.get', 'knowledge.principals.search', 'knowledge.search',
  'knowledge.sources.list', 'license.fingerprint', 'license.status',
  'localmodels.installed', 'localmodels.quants', 'localmodels.search',
  'mail.list', 'mail.outbox',
  'mail.status', 'maintenance.history', 'maintenance.status',
  'marketplace.list', 'mcp.events_list', 'mcp.list',
  'mcp.oauth.status', 'mcp.remote_status', 'mcp.tool_effects',
  'mcp_keys.list', 'mcp_oauth_grants.list', 'me.google.status',
  'memory.at', 'memory.browse', 'memory.decay_overview',
  'memory.forget_receipts_get', 'memory.forget_receipts_list', 'memory.graph',
  'memory.history', 'memory.key_facts', 'memory.search',
  'mfa.overview', 'mfa.status', 'models.list',
  'notify.stats', 'odoo.agent_config_get', 'odoo.config',
  'odoo.livechat.get', 'odoo.livechat.status', 'odoo.permission_options',
  'odoo.status', 'onboarding.get', 'os.events.recent',
  'os.gate.recent', 'os.status', 'plans.get',
  'plans.list', 'playbook.export', 'playbook.list',
  'presets.list', 'presets.status', 'redaction.custom_rules.list',
  'redaction.get', 'redaction.model.status', 'redaction.stats',
  'responsibilities.get', 'responsibilities.list', 'responsibilities.occurrences',
  'responsibilities.status', 'runs.get', 'runs.list',
  'secaudit.report', 'secaudit.reports', 'security.audit_log',
  'security.credential_hygiene', 'security.credential_inventory', 'security.egress_shadow_report',
  'security.status', 'sessions.policy', 'shared_wiki.pages',
  'shared_wiki.read', 'shared_wiki.search', 'shared_wiki.stats',
  'skill_synthesis.get', 'skills.archived', 'skills.content',
  'skills.leaderboard', 'skills.list', 'skills.recent',
  'skills.search', 'skills.shared', 'system.autostart.status',
  'system.config', 'system.product_profile', 'system.status',
  'system.version', 'takeover.list', 'task_forward_model.get',
  'tasks.artifacts', 'tasks.changes', 'tasks.comments',
  'tasks.flow_metrics', 'tasks.iterations', 'tasks.list',
  'tasks.list_page', 'tasks.role_turns', 'tasks.steering',
  'tasks.stop_status', 'tasks.timeline', 'templates.catalog',
  'templates.get', 'templates.industries', 'templates.role',
  'templates.roster', 'tick.sources.list', 'ticks.recent',
  'ticks.sources', 'timeline.list', 'tools.builtin_catalog',
  'tools.catalog', 'users.audit_log', 'users.list',
  'users.me', 'users.my_history_views', 'users.my_team',
  'users.org_settings.get', 'users.subordinates', 'users.team_history.get',
  'users.team_history.list', 'widgets.custom.get', 'widgets.custom.list',
  'wiki.auto_pages', 'wiki.lint', 'wiki.pages',
  'wiki.read', 'wiki.search', 'wiki.stats',
  'wiki.trust_audit', 'wiki.trust_history', 'wiki_scope.get',
]);

// mail.read marks mail as read; its normal params carry no agent id.
// Registration avoids a transport -> store -> api -> transport initialization cycle.
// There is no permissive fallback, including for a locally cached admin role.
interface AgentRequestAuthorizer {
  ensureFreshGrants: (sessionId?: string | null) => Promise<void>;
  canSelectAgent: (name: string, sessionId?: string | null) => boolean;
  identity: () => unknown;
}
let authorizer: AgentRequestAuthorizer = {
  ensureFreshGrants: async () => {},
  canSelectAgent: () => false,
  identity: () => null,
};
export function setAgentRequestAuthorizer(authorize: AgentRequestAuthorizer): void {
  authorizer = authorize;
}

// Exempt requests return synchronously and never wait for callable discovery.
export function withAgentRequestAllowed<T>(method: string, params: object, send: () => T, sessionId?: string | null): T | Promise<T> {
  if (AGENT_GUARD_BOOTSTRAP_METHODS.has(method) || AGENT_GUARD_READ_ONLY_METHODS.has(method)) return send();
  const targets: unknown[] = [];
  const check = (value: unknown) => {
    // Empty optional targets mean "no explicit agent"; the server resolves defaults.
    if (value == null || value === '') return;
    if (Array.isArray(value)) { value.forEach(check); return; }
    targets.push(value);
  };
  const visit = (value: unknown) => {
    if (!value || typeof value !== 'object') return;
    if (Array.isArray(value)) { value.forEach(visit); return; }
    const fields = value as Record<string, unknown>;
    for (const [key, target] of Object.entries(fields)) {
      // Plan steps may explicitly assign a HUMAN instead of an AI employee.
      if (key === 'assignee' && fields.assignee_kind === 'user' &&
          ['plans.create', 'plans.add_step', 'plans.update_step'].includes(method)) continue;
      if (AGENT_TARGET_KEYS.has(key)) check(target);
      else visit(target);
    }
  };
  visit(params);
  if (targets.length === 0) return send();
  return (async () => {
    for (;;) {
      const current = authorizer;
      const identity = current.identity();
      await current.ensureFreshGrants(sessionId);
      if (current !== authorizer || identity !== current.identity()) continue;
      if (targets.some(target => typeof target !== 'string' || !current.canSelectAgent(target, sessionId))) {
        throw new Error(`Agent access is unavailable or revoked; reload your employee permissions before ${method}.`);
      }
      // No await between the final identity check and starting the transport.
      if (current !== authorizer || identity !== current.identity()) continue;
      return send();
    }
  })();
}

export function assertAgentRequestAllowed(method: string, params: object, sessionId?: string | null): Promise<void> | void {
  return withAgentRequestAllowed(method, params, () => {}, sessionId);
}

/** HTTP clients share the same fence, including targets carried in query strings. */
export function withAgentHttpRequestAllowed<T>(path: string, init: RequestInit, send: () => T): T | Promise<T> {
  const verb = (init.method ?? 'GET').toUpperCase();
  if (verb === 'GET' || verb === 'HEAD' || verb === 'OPTIONS') return send();
  const url = new URL(path, 'http://dashboard.local');
  const method = `${verb} ${url.pathname}`;
  // Evaluate query and body targets together against the same identity/grants.
  const targets: object[] = [...url.searchParams].map(([key, value]) => ({ [key]: value }));
  if (typeof init.body === 'string') targets.push(JSON.parse(init.body) as object);
  return withAgentRequestAllowed(method, targets, send);
}

export function assertAgentHttpRequestAllowed(path: string, init: RequestInit): Promise<void> | void {
  return withAgentHttpRequestAllowed(path, init, () => {});
}
