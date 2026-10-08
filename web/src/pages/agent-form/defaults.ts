import type {
  AgentCapabilities,
  AgentEffort,
  AgentKvType,
  AgentTeamRoleKey,
  AgentEvolutionAdvanced,
  AgentRuntime,
  AutonomyLevel,
  ComputerUseConfig,
  RuntimeProvider,
  TemplateRoleSummary,
} from '@/lib/api';

// Shared form constants for the Create / Edit Agent pages. Moved out of
// AgentsPage.tsx when the two dialogs became standalone routes (/agents/new,
// /agents/:id/edit) so both pages source the same defaults.

/** Stable presentation order for template roles in the picker. */
export const TEMPLATE_KIND_ORDER: Record<TemplateRoleSummary['kind'], number> = {
  ceo: 0,
  front_desk: 1,
  worker: 2,
};

/** Two-level edit structure (spec §4.2): a top "一般 / 進階" split, and inside
 *  進階 a second-level tab strip of four groups. */
export type MainTab = 'general' | 'advanced';
export type AdvGroup = 'run' | 'access' | 'integration' | 'evo';

export const RUNTIME_PROVIDERS: ReadonlyArray<RuntimeProvider> = [
  'claude', 'codex', 'antigravity', 'grok', 'openai_compat',
  // v1.68 W1 — generic command-line runtimes (`runtime_catalog.rs`).
  'qwen', 'kimi', 'copilot', 'kiro', 'cursor', 'vibe', 'opencode',
];

/** v1.68 W1 — runtimes driven as plain command-line tools. Per-employee tool
 *  limits (allowed/denied tools, approval lists) do not reach them. */
export const GENERIC_CLI_RUNTIMES: ReadonlySet<string> = new Set([
  'qwen', 'kimi', 'copilot', 'kiro', 'cursor', 'vibe', 'opencode',
]);

export const AGENT_ROLES: ReadonlyArray<string> = ['main', 'specialist', 'worker', 'developer', 'qa', 'planner'];

/** Presentation order for the 自主等級 radio group — from the most
 *  human-driven to the most autonomous (mirrors `goal_loop::AutonomyLevel`'s
 *  declaration order, which is itself ordered least→most autonomous). */
export const AUTONOMY_LEVELS: ReadonlyArray<AutonomyLevel> = [
  'operator',
  'collaborator',
  'consultant',
  'approver',
  'observer',
];

/** RT — runtime form defaults. `agents.inspect` now returns the `[runtime]`
 *  block (only keys present in agent.toml), so the Edit page prefills these
 *  from `agent.runtime` when available and falls back to these defaults
 *  otherwise. A partial update is still written only when the operator
 *  touches the tab. */
export const DEFAULT_RUNTIME: Required<Pick<AgentRuntime, 'provider'>> & { fallback: string } = {
  provider: 'claude',
  fallback: '',
};

/** EVO — advanced evolution form defaults (write-only tab). */
export const DEFAULT_EVOLUTION_ADVANCED: {
  external_factors: Required<NonNullable<AgentEvolutionAdvanced['external_factors']>>;
  skill_synthesis_enabled: boolean;
  skill_synthesis_threshold: number;
  skill_synthesis_cooldown_hours: number;
  skill_trial_ttl: number;
  skill_graduation_min_lift: number;
} = {
  external_factors: {
    user_feedback: true,
    security_events: true,
    channel_metrics: false,
    business_context: false,
    peer_signals: false,
  },
  skill_synthesis_enabled: false,
  skill_synthesis_threshold: 3,
  skill_synthesis_cooldown_hours: 24,
  skill_trial_ttl: 7,
  skill_graduation_min_lift: 0.1,
};

/** Default capability values, used until agents.inspect prefills the form on
 *  tab open. A partial update is written only for fields the operator changed. */
export const DEFAULT_CAPABILITIES: Required<Omit<AgentCapabilities, 'computer_use_config'>> & {
  computer_use_config: Required<ComputerUseConfig>;
} = {
  computer_use: false,
  computer_use_mode: 'container',
  browser_via_bash: false,
  allowed_tools: [],
  denied_tools: [],
  wiki_visible_to: [],
  db_sources: [],
  approval_required_tools: [],
  irreversible_tools: [],
  maybe_irreversible_tools: [],
  scoped_tools: [],
  action_rules: [],
  native_sandbox: false,
  policy: [],
  os_native: false,
  recording: false,
  git_credentials: false,
  system_operator: false,
  codrive: false,
  // Conservative default — mirrors the gateway's fail-safe fallback
  // (`goal_loop::AutonomyLevel::for_agent`: missing/unparseable ⇒ Approver),
  // so an agent this form has never touched behaves the same before and
  // after the operator opens the 工具與權限 tab.
  autonomy_level: 'approver',
  computer_use_config: {
    allowed_apps: [],
    blocked_actions: [],
    max_session_minutes: 30,
    max_actions: 100,
    display_width: 1280,
    display_height: 800,
    auto_confirm_trusted: false,
    keep_alive_minutes: 0,
    takeover_idle_minutes: 10,
  },
};

/** v1.39 — default `[os_watch]` form values. Prefilled from agents.inspect
 *  (`os_watch`) on tab open; only written when the operator edits the section. */
export const DEFAULT_OS_WATCH: {
  paths: string[];
  ignore: string[];
  debounce_ms: number;
  max_events_per_min: number;
} = {
  paths: [],
  ignore: [],
  debounce_ms: 300,
  max_events_per_min: 60,
};

/** Belief loop × goal contract gap 2 — default `[research]` form values.
 *  Prefilled from agents.inspect (`research`) on tab open; only written when
 *  the operator edits the section. Unlike `[os_watch]`, `agents.inspect`
 *  always returns concrete values (no "unset" state to distinguish). */
export const DEFAULT_RESEARCH: {
  self_study: boolean;
  self_study_hour: number;
} = {
  self_study: false,
  self_study_hour: 20,
};

/** ODO — per-agent Odoo override (write-only tab; inspect doesn't return it). */
export const DEFAULT_ODOO: {
  profile: string;
  allowed_models: string[];
  allowed_actions: string[];
  company_ids: string; // comma-separated ints in the form
  url: string;
  db: string;
  username: string;
  api_key: string;
  password: string;
} = {
  profile: '',
  allowed_models: [],
  allowed_actions: [],
  company_ids: '',
  url: '',
  db: '',
  username: '',
  api_key: '',
  password: '',
};

/** Advanced typed key/value editor row (v1.68 W1). `value` is the raw text
 *  the operator typed; `type` decides how it is parsed before sending. */
export interface KvRow { section: string; key: string; value: string; type: AgentKvType }
export const DEFAULT_ADVANCED: {
  account_pool: string[];
  utility: string;
  heartbeat_max_concurrent_runs: number;
  heartbeat_cron_timezone: string;
  proactive_timezone: string;
  /** null = not set in agent.toml (the gateway default applies). */
  proactive_max_turns: number | null;
  /** [proactive] notify target — goal-loop + skill-digest push destination.
   *  Prefilled from agents.inspect (unlike the write-only extras above). */
  proactive_notify_channel: string;
  proactive_notify_chat_id: string;
  proactive_notify_thread_id: string;
  /** W2-8 — [proactive] quiet_hours (`HH:MM-HH:MM`), empty = not set.
   *  Prefilled from `agents.inspect`'s `proactive.quiet_hours_own` (the
   *  agent's OWN raw value, never the fallen-back effective one — see
   *  `ProactiveSettings.quiet_hours_own` in `lib/api.ts`). */
  proactive_quiet_hours: string;
  kv: KvRow[];
} = {
  account_pool: [],
  utility: '',
  heartbeat_max_concurrent_runs: 1,
  heartbeat_cron_timezone: '',
  proactive_timezone: '',
  proactive_max_turns: null,
  proactive_notify_channel: '',
  proactive_notify_chat_id: '',
  proactive_notify_thread_id: '',
  proactive_quiet_hours: '',
  kv: [],
};

/** v1.68 W1 — sections the advanced editor suggests (all have a reader). */
export const KV_SECTION_SUGGESTIONS: ReadonlyArray<string> = [
  'prompt', 'fork', 'night_engine', 'evolution', 'budget', 'planner', 'goal_intent', 'skills',
];

export const EFFORT_LEVELS: ReadonlyArray<Exclude<AgentEffort, ''>> = ['low', 'medium', 'high', 'xhigh', 'max'];

export const TEAM_ROLE_KEYS: ReadonlyArray<AgentTeamRoleKey> = ['planner', 'executor', 'verifier', 'utility'];

export type TeamRoleForm = { runtime: string; model: string; effort: string };

/** v1.68 W1 — the new per-employee controls. Shown empty/default until
 *  `agents.inspect` prefills them; written only when the operator edits one. */
export const DEFAULT_V168: {
  daily_cap_cents: number;
  effort: AgentEffort;
  minimal_context: boolean;
  fork_enabled: boolean;
  team_enabled: boolean;
  team_roles: Record<AgentTeamRoleKey, TeamRoleForm>;
  guardrails_enabled: boolean;
  guardrails_block_secrets: boolean;
  guardrails_block_injection_echo: boolean;
  guardrails_redact_pii: boolean;
  guardrails_deny_phrases: string[];
  decision_continuity: boolean;
  decision_ttl_days: number;
  night_engine_enabled: boolean;
} = {
  daily_cap_cents: 0,
  effort: '',
  minimal_context: true,
  fork_enabled: false,
  team_enabled: true,
  team_roles: {
    planner: { runtime: '', model: '', effort: '' },
    executor: { runtime: '', model: '', effort: '' },
    verifier: { runtime: '', model: '', effort: '' },
    utility: { runtime: '', model: '', effort: '' },
  },
  guardrails_enabled: false,
  guardrails_block_secrets: true,
  guardrails_block_injection_echo: true,
  guardrails_redact_pii: false,
  guardrails_deny_phrases: [],
  decision_continuity: false,
  decision_ttl_days: 7,
  night_engine_enabled: false,
};
