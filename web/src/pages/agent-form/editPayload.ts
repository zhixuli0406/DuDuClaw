import type {
  AgentCapabilities,
  AgentEffort,
  AgentKvEntry,
  AgentKvType,
  AgentOdooOverride,
  AgentTeamRoleKey,
  AgentTeamRoleSpec,
  AgentUpdateParams,
  AgentUpdateParamsV168,
  ComputerUseConfig,
} from '@/lib/api';
import {
  type KvRow,
  type TeamRoleForm,
  DEFAULT_CAPABILITIES,
  DEFAULT_EVOLUTION_ADVANCED,
  DEFAULT_OS_WATCH,
  DEFAULT_RESEARCH,
  DEFAULT_RUNTIME,
  DEFAULT_ADVANCED,
  DEFAULT_ODOO,
  DEFAULT_V168,
  TEAM_ROLE_KEYS,
} from './defaults';

/**
 * v1.68 W1 — partial-write payload builder for the AI-employee edit page.
 *
 * The page records a dirty path for every field the operator edits
 * (`form.display_name`, `caps.computer_use_config.max_actions`,
 * `v.team_roles.executor.model`, `kv` …). Only dirty paths reach the payload,
 * so a saved value the form never loaded (an older gateway, a section the
 * operator never opened) is never overwritten with a form default — the
 * audit's F1/F2 clobber class. The gateway treats every section as partial.
 */

export type OdooFormState = typeof DEFAULT_ODOO & {
  unblock_models: string[];
  clear_api_key: boolean;
  clear_password: boolean;
};

export type V168State = typeof DEFAULT_V168;

export interface EditFormState {
  form: AgentUpdateParams;
  caps: typeof DEFAULT_CAPABILITIES;
  osWatch: typeof DEFAULT_OS_WATCH;
  research: typeof DEFAULT_RESEARCH;
  runtime: typeof DEFAULT_RUNTIME;
  evoAdv: typeof DEFAULT_EVOLUTION_ADVANCED;
  odoo: OdooFormState;
  adv: typeof DEFAULT_ADVANCED;
  v: V168State;
}

export interface PayloadContext {
  /** The agent's saved cloud preferred/fallback (never `local:` ids). */
  savedCloudPreferred: string;
  savedCloudFallback: string;
  /** Whether the agent already has a `[model.local]` table. */
  hasSavedLocal: boolean;
  /** First cloud model the registry reports, '' when none. */
  firstCloudModel: string;
}

export type FullUpdatePayload = AgentUpdateParams & AgentUpdateParamsV168;

// ── Typed key/value rows ─────────────────────────────────────────────

// Mirrors the gateway: lowercase key, section path of at most 3 segments.
const KV_SECTION_RE = /^[a-z][a-z0-9_]*(\.[a-z0-9_]+){0,2}$/;
const KV_KEY_RE = /^[a-z0-9_]+$/;

/** Sections the gateway refuses in the typed key/value write (they have
 *  dedicated controls, are protected, or have no reader). */
export const KV_REFUSED_SECTIONS: ReadonlySet<string> = new Set([
  'ptc', 'cultural_context', 'sticker', 'agent', 'capabilities', 'container', 'permissions', 'channels', 'odoo', 'mcp', 'runtime',
]);

export type KvRowError = 'section' | 'key' | 'value';

export type KvParse =
  | { kind: 'empty' }
  | { kind: 'ok'; entry: AgentKvEntry }
  | { kind: 'error'; error: KvRowError };

/** Parse one editor row into a typed entry. A row with neither key nor value
 *  is "empty" (ignored); anything else must parse fully or it is an error. */
export function parseKvRow(row: KvRow): KvParse {
  const section = row.section.trim();
  const key = row.key.trim();
  const raw = row.value.trim();
  if (key === '' && raw === '') return { kind: 'empty' };
  if (!KV_SECTION_RE.test(section) || KV_REFUSED_SECTIONS.has(section.split('.')[0])) {
    return { kind: 'error', error: 'section' };
  }
  if (!KV_KEY_RE.test(key)) return { kind: 'error', error: 'key' };
  const value = parseKvValue(raw, row.type);
  if (value === undefined) return { kind: 'error', error: 'value' };
  return { kind: 'ok', entry: { section, key, value, type: row.type } };
}

export function parseKvValue(raw: string, type: AgentKvType): AgentKvEntry['value'] | undefined {
  switch (type) {
    case 'string':
      return raw;
    case 'integer':
      return /^-?\d+$/.test(raw) && Number.isSafeInteger(Number(raw)) ? Number(raw) : undefined;
    case 'float': {
      if (raw === '' || !/^-?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/.test(raw)) return undefined;
      const n = Number(raw);
      return Number.isFinite(n) ? n : undefined;
    }
    case 'boolean':
      return raw === 'true' ? true : raw === 'false' ? false : undefined;
    case 'string_array':
      return raw
        .split(/[\n,]/)
        .map((s) => s.trim())
        .filter((s) => s !== '');
    default:
      return undefined;
  }
}

/** Build editor rows from a raw TOML table returned by inspect. */
export function kvRowsFromTable(section: string, table: Record<string, unknown> | undefined | null): KvRow[] {
  if (!table || typeof table !== 'object') return [];
  const rows: KvRow[] = [];
  for (const [key, value] of Object.entries(table)) {
    if (typeof value === 'boolean') rows.push({ section, key, value: String(value), type: 'boolean' });
    else if (typeof value === 'number')
      rows.push({ section, key, value: String(value), type: Number.isInteger(value) ? 'integer' : 'float' });
    else if (typeof value === 'string') rows.push({ section, key, value, type: 'string' });
    else if (Array.isArray(value) && value.every((x) => typeof x === 'string'))
      rows.push({ section, key, value: value.join(', '), type: 'string_array' });
    // Nested tables / mixed arrays are not editable here; they stay untouched
    // on disk because nothing about them is ever sent.
  }
  return rows;
}

// ── Payload ──────────────────────────────────────────────────────────

/** Flat top-level `form.*` keys that map 1:1 onto an `agents.update` param. */
const FLAT_FORM_KEYS: ReadonlyArray<keyof AgentUpdateParams> = [
  'display_name', 'role', 'status', 'trigger', 'icon', 'reports_to', 'department',
  'api_mode', 'use_router',
  'monthly_limit_cents', 'warn_threshold_percent', 'hard_stop',
  'heartbeat_enabled', 'heartbeat_interval', 'heartbeat_cron',
  'can_create_agents', 'can_send_cross_agent', 'can_modify_own_skills', 'can_modify_own_soul', 'can_schedule_tasks',
  'timeout_ms', 'sandbox_enabled', 'network_access',
  'gvu_enabled', 'max_active_skills', 'max_silence_hours', 'skill_token_budget',
];

const CAP_LIST_KEYS: ReadonlyArray<keyof AgentCapabilities> = [
  'computer_use', 'computer_use_mode', 'browser_via_bash', 'allowed_tools', 'denied_tools', 'wiki_visible_to',
  'db_sources', 'native_sandbox', 'policy', 'os_native', 'recording', 'git_credentials', 'system_operator',
  'codrive', 'autonomy_level',
  'approval_required_tools', 'irreversible_tools', 'maybe_irreversible_tools', 'scoped_tools',
];

const COMPUTER_USE_CONFIG_KEYS: ReadonlyArray<keyof ComputerUseConfig> = [
  'allowed_apps', 'blocked_actions', 'max_session_minutes', 'max_actions', 'display_width', 'display_height',
  'auto_confirm_trusted',
];

/** Dirty paths that touch the admin-only surface (backend refuses + audits
 *  them for non-admins). Used to drop them client-side for non-admin users. */
export function isAdminOnlyPath(path: string): boolean {
  return (
    path === 'form.reports_to' ||
    path === 'form.department' ||
    path === 'form.can_modify_own_soul' ||
    path === 'form.sandbox_enabled' ||
    path === 'form.network_access' ||
    path.startsWith('caps.')
  );
}

function has(dirty: ReadonlySet<string>, path: string): boolean {
  return dirty.has(path);
}

function anyWithPrefix(dirty: ReadonlySet<string>, prefix: string): boolean {
  for (const p of dirty) if (p.startsWith(prefix)) return true;
  return false;
}

export interface BuildResult {
  payload: FullUpdatePayload;
  /** True when at least one field is in the payload. */
  hasChanges: boolean;
  /** True when the payload carries `advanced_kv` (for routing a save error). */
  includesKv: boolean;
}

export function buildUpdatePayload(
  s: EditFormState,
  dirty: ReadonlySet<string>,
  ctx: PayloadContext,
): BuildResult {
  const out: FullUpdatePayload = {};
  const put = <K extends keyof FullUpdatePayload>(k: K, v: FullUpdatePayload[K]) => {
    out[k] = v;
  };

  for (const k of FLAT_FORM_KEYS) {
    if (has(dirty, `form.${String(k)}`)) put(k, s.form[k] as never);
  }

  // Model ids: decompose `local:` choices into the local table only when the
  // operator touched the preferred/fallback pickers.
  if (has(dirty, 'form.preferred') || has(dirty, 'form.fallback')) {
    const pref = s.form.preferred ?? '';
    const fb = s.form.fallback ?? '';
    const cloudPref = ctx.savedCloudPreferred || ctx.firstCloudModel;
    const cloudFb = ctx.savedCloudFallback || ctx.firstCloudModel;
    let localModel = '';
    let preferred = pref;
    let fallback = fb;
    const preferLocal = pref.startsWith('local:');
    if (preferLocal) {
      localModel = pref.slice('local:'.length);
      preferred = fb.startsWith('local:') ? cloudPref : fb || cloudPref;
    }
    if (fb.startsWith('local:')) {
      localModel = localModel || fb.slice('local:'.length);
      fallback = cloudFb;
    }
    if (has(dirty, 'form.preferred') || preferLocal) put('preferred', preferred);
    if (has(dirty, 'form.fallback') || fb.startsWith('local:')) put('fallback', fallback);
    if (localModel !== '') put('local_model', localModel);
    // Only touch prefer_local when a local table exists or is being created,
    // so a cloud-only employee never gains an empty `[model.local]`.
    if (preferLocal || ctx.hasSavedLocal) put('prefer_local', preferLocal);
  }

  // [capabilities] — only the edited keys.
  const caps: AgentCapabilities = {};
  for (const k of CAP_LIST_KEYS) {
    if (has(dirty, `caps.${String(k)}`)) (caps as Record<string, unknown>)[k] = s.caps[k as keyof typeof s.caps];
  }
  const cuc: ComputerUseConfig = {};
  for (const k of COMPUTER_USE_CONFIG_KEYS) {
    if (has(dirty, `caps.computer_use_config.${String(k)}`)) {
      (cuc as Record<string, unknown>)[k] = s.caps.computer_use_config[k];
    }
  }
  if (Object.keys(cuc).length > 0) caps.computer_use_config = cuc;
  if (Object.keys(caps).length > 0) put('capabilities', caps);

  // [os_watch]
  if (anyWithPrefix(dirty, 'osWatch.')) {
    const ow: Record<string, unknown> = {};
    for (const k of Object.keys(DEFAULT_OS_WATCH) as Array<keyof typeof DEFAULT_OS_WATCH>) {
      if (has(dirty, `osWatch.${k}`)) ow[k] = s.osWatch[k];
    }
    put('os_watch', ow);
  }

  // [research]
  if (anyWithPrefix(dirty, 'research.')) {
    const rs: Record<string, unknown> = {};
    for (const k of Object.keys(DEFAULT_RESEARCH) as Array<keyof typeof DEFAULT_RESEARCH>) {
      if (has(dirty, `research.${k}`)) rs[k] = s.research[k];
    }
    put('research', rs);
  }

  // [runtime] — provider/fallback plus minimal_context.
  const rt: Record<string, unknown> = {};
  if (has(dirty, 'runtime.provider')) rt.provider = s.runtime.provider;
  if (has(dirty, 'runtime.fallback')) rt.fallback = s.runtime.fallback;
  if (has(dirty, 'v.minimal_context')) rt.minimal_context = s.v.minimal_context;
  if (Object.keys(rt).length > 0) put('runtime', rt);

  // [evolution.*] advanced
  if (anyWithPrefix(dirty, 'evo.')) {
    const evo: Record<string, unknown> = {};
    const factors: Record<string, boolean> = {};
    for (const k of Object.keys(DEFAULT_EVOLUTION_ADVANCED.external_factors) as Array<
      keyof typeof DEFAULT_EVOLUTION_ADVANCED.external_factors
    >) {
      if (has(dirty, `evo.external_factors.${k}`)) factors[k] = s.evoAdv.external_factors[k];
    }
    if (Object.keys(factors).length > 0) evo.external_factors = factors;
    for (const k of [
      'skill_synthesis_enabled',
      'skill_synthesis_threshold',
      'skill_synthesis_cooldown_hours',
      'skill_trial_ttl',
      'skill_graduation_min_lift',
    ] as const) {
      if (has(dirty, `evo.${k}`)) evo[k] = s.evoAdv[k];
    }
    put('evolution_advanced', evo);
  }

  // [odoo] — edited keys only; secrets are write-only.
  if (anyWithPrefix(dirty, 'odoo.')) {
    const o: AgentOdooOverride & { unblock_models?: string[] } = {};
    for (const k of ['profile', 'allowed_models', 'unblock_models', 'allowed_actions', 'url', 'db', 'username'] as const) {
      if (has(dirty, `odoo.${k}`)) (o as Record<string, unknown>)[k] = s.odoo[k];
    }
    if (has(dirty, 'odoo.company_ids')) {
      o.company_ids = s.odoo.company_ids
        .split(',')
        .map((x) => x.trim())
        .filter((x) => x !== '')
        .map((x) => Number(x))
        .filter((n) => Number.isInteger(n) && n >= 0);
    }
    if (s.odoo.clear_api_key) o.api_key = '';
    else if (has(dirty, 'odoo.api_key') && s.odoo.api_key.trim() !== '') o.api_key = s.odoo.api_key;
    if (s.odoo.clear_password) o.password = '';
    else if (has(dirty, 'odoo.password') && s.odoo.password.trim() !== '') o.password = s.odoo.password;
    if (Object.keys(o).length > 0) put('odoo', o);
  }

  // Scattered advanced fields.
  if (has(dirty, 'adv.account_pool')) put('account_pool', s.adv.account_pool);
  if (has(dirty, 'adv.utility')) put('utility', s.adv.utility);
  if (has(dirty, 'adv.heartbeat_max_concurrent_runs')) put('heartbeat_max_concurrent_runs', s.adv.heartbeat_max_concurrent_runs);
  // Time zones: an empty value is not sent (the gateway would store "").
  if (has(dirty, 'adv.heartbeat_cron_timezone') && s.adv.heartbeat_cron_timezone.trim() !== '') {
    put('heartbeat_cron_timezone', s.adv.heartbeat_cron_timezone.trim());
  }
  const pro: NonNullable<AgentUpdateParams['proactive']> = {};
  if (has(dirty, 'adv.proactive_max_turns') && s.adv.proactive_max_turns !== null) pro.max_turns = s.adv.proactive_max_turns;
  if (has(dirty, 'adv.proactive_timezone') && s.adv.proactive_timezone.trim() !== '') pro.timezone = s.adv.proactive_timezone.trim();
  if (has(dirty, 'adv.proactive_notify_channel')) pro.notify_channel = s.adv.proactive_notify_channel;
  if (has(dirty, 'adv.proactive_notify_chat_id')) pro.notify_chat_id = s.adv.proactive_notify_chat_id.trim();
  if (has(dirty, 'adv.proactive_notify_thread_id')) pro.notify_thread_id = s.adv.proactive_notify_thread_id.trim();
  if (has(dirty, 'adv.proactive_quiet_hours')) pro.quiet_hours = s.adv.proactive_quiet_hours.trim();
  if (Object.keys(pro).length > 0) put('proactive', pro);
  // ── v1.68 sections ──
  if (has(dirty, 'v.daily_cap_cents')) put('budget', { daily_cap_cents: Math.max(0, Math.round(s.v.daily_cap_cents)) });
  if (has(dirty, 'v.effort')) put('model', { effort: s.v.effort as AgentEffort });
  if (has(dirty, 'v.fork_enabled')) put('fork', { enabled: s.v.fork_enabled });
  if (has(dirty, 'v.night_engine_enabled')) put('night_engine', { enabled: s.v.night_engine_enabled });

  const mem: Record<string, unknown> = {};
  if (has(dirty, 'v.decision_continuity')) mem.decision_continuity = s.v.decision_continuity;
  if (has(dirty, 'v.decision_ttl_days')) mem.decision_ttl_days = Math.max(1, Math.round(s.v.decision_ttl_days));
  if (Object.keys(mem).length > 0) put('memory', mem);

  const gr: Record<string, unknown> = {};
  if (has(dirty, 'v.guardrails_enabled')) gr.enabled = s.v.guardrails_enabled;
  if (has(dirty, 'v.guardrails_block_secrets')) gr.block_secrets = s.v.guardrails_block_secrets;
  if (has(dirty, 'v.guardrails_block_injection_echo')) gr.block_injection_echo = s.v.guardrails_block_injection_echo;
  if (has(dirty, 'v.guardrails_redact_pii')) gr.redact_pii = s.v.guardrails_redact_pii;
  if (has(dirty, 'v.guardrails_deny_phrases')) gr.deny_phrases = s.v.guardrails_deny_phrases;
  if (Object.keys(gr).length > 0) put('guardrails', gr);

  const team: { enabled?: boolean; roles?: Partial<Record<AgentTeamRoleKey, AgentTeamRoleSpec>> } = {};
  if (has(dirty, 'v.team_enabled')) team.enabled = s.v.team_enabled;
  for (const role of TEAM_ROLE_KEYS) {
    const spec: AgentTeamRoleSpec = {};
    for (const f of ['runtime', 'model', 'effort'] as Array<keyof TeamRoleForm>) {
      if (has(dirty, `v.team_roles.${role}.${f}`)) spec[f] = s.v.team_roles[role][f];
    }
    if (Object.keys(spec).length > 0) team.roles = { ...(team.roles ?? {}), [role]: spec };
  }
  if (Object.keys(team).length > 0) put('team', team);

  let includesKv = false;
  if (has(dirty, 'kv')) {
    const entries: AgentKvEntry[] = [];
    for (const row of s.adv.kv) {
      const p = parseKvRow(row);
      if (p.kind === 'ok') entries.push(p.entry);
    }
    if (entries.length > 0) {
      put('advanced_kv', entries);
      includesKv = true;
    }
  }

  return { payload: out, hasChanges: Object.keys(out).length > 0, includesKv };
}

/** Whether executor and verifier are bound in a way the gateway will refuse
 *  (same vendor ⇒ the team never forms). Client-side hint only; the gateway
 *  validates model families authoritatively. */
export function teamRolesShareVendor(roles: Record<AgentTeamRoleKey, TeamRoleForm>): boolean {
  const ex = roles.executor.runtime.trim();
  const ve = roles.verifier.runtime.trim();
  if (ex === '' || ve === '') return false;
  // openai_compat can front any vendor — only a matching model id is a
  // confident same-vendor signal there.
  if (ex === 'openai_compat' && ve === 'openai_compat') {
    const em = roles.executor.model.trim();
    return em !== '' && em === roles.verifier.model.trim();
  }
  return ex === ve;
}
