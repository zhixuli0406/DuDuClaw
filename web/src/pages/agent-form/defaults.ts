import type {
  AgentCapabilities,
  AgentEvolutionAdvanced,
  AgentRuntime,
  ComputerUseConfig,
  ContainerEnvVar,
  ContainerMount,
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

export const RUNTIME_PROVIDERS: ReadonlyArray<RuntimeProvider> = ['claude', 'codex', 'gemini', 'grok', 'openai_compat'];

export const AGENT_ROLES: ReadonlyArray<string> = ['main', 'specialist', 'worker', 'developer', 'qa', 'planner'];

/** RT — runtime form defaults. `agents.inspect` now returns the `[runtime]`
 *  block (only keys present in agent.toml), so the Edit page prefills these
 *  from `agent.runtime` when available and falls back to these defaults
 *  otherwise. A partial update is still written only when the operator (or the
 *  PTY-pool OAuth default-enable) touches the tab. */
export const DEFAULT_RUNTIME: Required<Omit<AgentRuntime, 'fallback'>> & { fallback: string } = {
  provider: 'claude',
  fallback: '',
  pty_pool_enabled: false,
  worker_managed: false,
};

/** EVO — advanced evolution form defaults (write-only tab). */
export const DEFAULT_EVOLUTION_ADVANCED: {
  external_factors: Required<NonNullable<AgentEvolutionAdvanced['external_factors']>>;
  skill_synthesis_enabled: boolean;
  skill_synthesis_threshold: number;
  skill_synthesis_cooldown_hours: number;
  skill_trial_ttl: number;
  skill_graduation_enabled: boolean;
  skill_graduation_min_lift: number;
  skill_recommendation_enabled: boolean;
  skill_recommendation_threshold: number;
  curiosity_enabled: boolean;
  curiosity_threshold: number;
  curiosity_max_daily: number;
  skill_behavior_monitor_enabled: boolean;
  skill_behavior_drift_threshold: number;
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
  skill_graduation_enabled: false,
  skill_graduation_min_lift: 0.1,
  skill_recommendation_enabled: false,
  skill_recommendation_threshold: 0.6,
  curiosity_enabled: false,
  curiosity_threshold: 0.5,
  curiosity_max_daily: 3,
  skill_behavior_monitor_enabled: false,
  skill_behavior_drift_threshold: 0.3,
};

/** CT — advanced container form defaults (write-only tab). */
export const DEFAULT_CONTAINER_ADVANCED: {
  worktree_enabled: boolean;
  worktree_auto_merge: boolean;
  worktree_cleanup_on_exit: boolean;
  worktree_copy_files: string[];
  additional_mounts: ContainerMount[];
  cmd: string[];
  env: ContainerEnvVar[];
} = {
  worktree_enabled: false,
  worktree_auto_merge: false,
  worktree_cleanup_on_exit: true,
  worktree_copy_files: [],
  additional_mounts: [],
  cmd: [],
  env: [],
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
  native_sandbox: false,
  policy: [],
  computer_use_config: {
    allowed_apps: [],
    blocked_actions: [],
    max_session_minutes: 30,
    max_actions: 100,
    display_width: 1280,
    display_height: 800,
    auto_confirm_trusted: false,
  },
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

/** Advanced (G.8 free-form scalar tables) — write-only. Stored as KV rows. */
export interface KvRow { key: string; value: string }
export const DEFAULT_ADVANCED: {
  account_pool: string[];
  utility: string;
  heartbeat_max_concurrent_runs: number;
  heartbeat_cron_timezone: string;
  proactive_token_budget_per_check: number;
  proactive_timezone: string;
  proactive_max_turns: number;
  stagnation_enabled: boolean;
  stagnation_window_seconds: number;
  stagnation_trigger_threshold: number;
  stagnation_action: 'log_only' | 'suppress';
  ptc: KvRow[];
  prompt: KvRow[];
  cultural_context: KvRow[];
} = {
  account_pool: [],
  utility: '',
  heartbeat_max_concurrent_runs: 1,
  heartbeat_cron_timezone: '',
  proactive_token_budget_per_check: 0,
  proactive_timezone: '',
  proactive_max_turns: 1,
  stagnation_enabled: false,
  stagnation_window_seconds: 3600,
  stagnation_trigger_threshold: 3,
  stagnation_action: 'log_only',
  ptc: [],
  prompt: [],
  cultural_context: [],
};
