/**
 * Autopilot rule wire contract + the dashboard rule form's payload builder.
 *
 * Every constant here MIRRORS a Rust source of truth. When the server side
 * changes, change this file in the same commit:
 *
 *  - `SERVER_TRIGGER_EVENTS`  ← `AUTOPILOT_CREATE_TRIGGER_EVENTS` in
 *    `crates/duduclaw-gateway/src/handlers/task_json.rs` (what a NEW rule may
 *    use). A Rust test (`autopilot_validation_tests`) parses this array, so
 *    keep it a plain array literal of string literals.
 *  - `LEGACY_TRIGGER_EVENTS`  ← `AUTOPILOT_LEGACY_TRIGGER_EVENTS` (refused on
 *    create, still accepted when a stored rule is updated; display/edit only).
 *  - `CONDITION_OPS`          ← `autopilot_engine::CONDITION_OPS`
 *    (`crates/duduclaw-gateway/src/autopilot_engine.rs`).
 *  - `ACTION_REQUIRED_FIELDS` ← `validate_autopilot_action` in `task_json.rs`.
 *  - `NOTIFY_CHANNELS`        ← `AutopilotEngine::action_notify` +
 *    `channel_sender::resolve_channel_target` (webchat is refused there).
 *  - `TRIGGER_FIELD_SUGGESTIONS` ← `AutopilotEvent::to_fields` in
 *    `autopilot_engine.rs` (task / activity payloads come from
 *    `task_row_to_json` / `activity_row_to_json` in `task_json.rs`).
 *
 * Conditions: an empty set (`{}`, absent, `null`, or `{ "all": [] }`) means
 * "always matches when the trigger fires". Malformed conditions are refused
 * on save with a message naming the path (`validate_autopilot_conditions`).
 * The form sends an explicit `{ all: [...] }` / `{ any: [...] }` group.
 * Action templates interpolate `{field.subfield}` (single braces).
 */

// ── Wire types ──────────────────────────────────────────────

export const SERVER_TRIGGER_EVENTS = [
  'task_created',
  'task_updated',
  'task_status_changed',
  'activity_new',
  'channel_message',
  'agent_idle',
  'run_at_risk',
  'os_file',
  'os_frontmost',
  'tick',
  'security_event',
  'odoo_event',
  'mcp_event',
] as const;

/**
 * Accepted only on stored rules. `cron_tick` is never emitted, so a rule on
 * it never fires; scheduled work belongs to the cron scheduler (the 例行工作
 * page, `/routines`). Shown and editable on existing rules, never offered.
 */
export const LEGACY_TRIGGER_EVENTS = ['cron_tick'] as const;

/** A trigger a new rule may use. */
export type AutopilotCreateTriggerEvent = (typeof SERVER_TRIGGER_EVENTS)[number];
/** Any trigger a stored rule may carry. */
export type AutopilotTriggerEvent =
  | AutopilotCreateTriggerEvent
  | (typeof LEGACY_TRIGGER_EVENTS)[number];

/** Triggers the rule form offers. */
export const FORM_TRIGGER_EVENTS: readonly AutopilotCreateTriggerEvent[] = SERVER_TRIGGER_EVENTS;

export const CONDITION_OPS = [
  'eq',
  'neq',
  'in',
  'not_in',
  'gt',
  'gte',
  'lt',
  'lte',
  'contains',
] as const;

export type ConditionOp = (typeof CONDITION_OPS)[number];

export interface AutopilotConditionLeaf {
  field: string;
  op: string;
  value: unknown;
}

/** Engine condition tree: a leaf, an `all` / `any` group, or `null` (always). */
export type AutopilotCondition =
  | AutopilotConditionLeaf
  | { all: AutopilotCondition[] }
  | { any: AutopilotCondition[] }
  | Record<string, unknown>
  | null;

/** Action types the server accepts (`proactive_notify` is not offered in the form). */
export type AutopilotActionType = 'delegate' | 'notify' | 'run_skill' | 'proactive_notify';

export interface AutopilotAction {
  type: AutopilotActionType | string;
  target_agent?: string;
  prompt?: string;
  skill_name?: string;
  channel?: string;
  chat_id?: string;
  text?: string;
  /** Legacy key written by the pre-v1.67.1 form; the server never read it. */
  agent_id?: string;
  /** Optional extras (`screen`, `context_ticks`, …) the form keeps verbatim. */
  [key: string]: unknown;
}

export interface AutopilotRule {
  id: string;
  name: string;
  enabled: boolean;
  trigger_event: AutopilotTriggerEvent | string;
  conditions: AutopilotCondition;
  action: AutopilotAction;
  created_at: string;
  last_triggered_at?: string | null;
  trigger_count: number;
  /** Present only for event-sequence rules; null otherwise. */
  sequence?: unknown;
  /** Provenance of rules induced from behaviour; null for hand-authored rules. */
  metadata?: unknown;
}

export interface AutopilotCreateParams {
  name: string;
  trigger_event: AutopilotCreateTriggerEvent;
  conditions: AutopilotCondition;
  action: AutopilotAction;
}

export interface AutopilotUpdateFields {
  name?: string;
  trigger_event?: AutopilotTriggerEvent | string;
  conditions?: AutopilotCondition;
  action?: AutopilotAction;
  enabled?: boolean;
}

// ── Form vocabulary ─────────────────────────────────────────

export const FORM_ACTION_TYPES = ['delegate', 'notify', 'run_skill'] as const;
export type FormActionType = (typeof FORM_ACTION_TYPES)[number];

export const ACTION_REQUIRED_FIELDS: Record<FormActionType, readonly string[]> = {
  delegate: ['target_agent', 'prompt'],
  notify: ['channel', 'chat_id', 'text'],
  run_skill: ['target_agent', 'skill_name'],
};

/** Every key the form owns on an action object; anything else is kept as-is. */
const FORM_OWNED_ACTION_KEYS = new Set([
  'type',
  'agent_id',
  'prompt_template',
  ...Object.values(ACTION_REQUIRED_FIELDS).flat(),
]);

export const NOTIFY_CHANNELS = [
  'telegram',
  'line',
  'discord',
  'slack',
  'whatsapp',
  'feishu',
  'googlechat',
  'teams',
  'wecom',
  'dingtalk',
] as const;

/** Field names each event exposes to conditions (suggestions; any name is accepted). */
export const TRIGGER_FIELD_SUGGESTIONS: Record<AutopilotTriggerEvent, readonly string[]> = {
  task_created: ['task.title', 'task.priority', 'task.status', 'task.assigned_to', 'task.tags'],
  task_updated: ['task.title', 'task.priority', 'task.status', 'task.assigned_to', 'task.tags'],
  task_status_changed: ['from', 'to', 'task.priority', 'task.assigned_to', 'task.title'],
  activity_new: ['activity.type', 'activity.agent_id', 'activity.summary'],
  channel_message: ['text', 'channel', 'agent_id'],
  agent_idle: ['agent_id', 'idle_minutes'],
  cron_tick: ['now'],
  run_at_risk: ['agent_id', 'level', 'score', 'reasons'],
  os_file: ['agent_id', 'kind', 'extension', 'file_name', 'path'],
  os_frontmost: ['agent_id', 'app', 'window_title', 'prev_app'],
  tick: ['source'],
  security_event: ['severity', 'event_type', 'agent_id', 'source'],
  odoo_event: ['event_type', 'model', 'record_id'],
  mcp_event: ['server', 'name', 'agent_id', 'lane', 'suspicious'],
};

/** Triggers whose interesting fields are defined by the operator's own data. */
export const FREE_FORM_FIELD_TRIGGERS: ReadonlySet<string> = new Set(['tick', 'odoo_event', 'mcp_event']);

// ── Form state ──────────────────────────────────────────────

export interface ConditionRow {
  field: string;
  op: ConditionOp;
  /** What the person typed (lists are comma separated). */
  valueText: string;
  /**
   * The exact value loaded from a saved rule. Kept until the person edits
   * this row's operator or value, so an untouched `"3"` stays a string.
   */
  typedValue?: unknown;
}

export interface RuleFormState {
  name: string;
  triggerEvent: string;
  match: 'all' | 'any';
  conditions: ConditionRow[];
  /** Set when the saved conditions cannot be shown as a flat list; never re-sent. */
  conditionsLocked: boolean;
  /** Set once the person changes any condition (edit mode sends conditions only then). */
  conditionsTouched: boolean;
  /** Set when the saved action is not one of the three form actions; never re-sent. */
  actionLocked: boolean;
  /** Event-sequence rules: trigger and conditions are not the form's to change. */
  sequenceRule: boolean;
  actionType: FormActionType;
  targetAgent: string;
  prompt: string;
  skillName: string;
  channel: string;
  chatId: string;
  text: string;
}

export function emptyRuleForm(defaultAgent = ''): RuleFormState {
  return {
    name: '',
    triggerEvent: 'task_created',
    match: 'all',
    conditions: [],
    conditionsLocked: false,
    conditionsTouched: false,
    actionLocked: false,
    sequenceRule: false,
    actionType: 'delegate',
    targetAgent: defaultAgent,
    prompt: '',
    skillName: '',
    channel: 'telegram',
    chatId: '',
    text: '',
  };
}

// ── Value parsing ───────────────────────────────────────────

export type BuildIssue =
  | { kind: 'missing'; field: string }
  | { kind: 'conditionField'; row: number }
  | { kind: 'conditionValue'; row: number }
  | { kind: 'conditionNumber'; row: number };

const NUMBER_RE = /^-?\d+(\.\d+)?$/;
const LIST_SPLIT_RE = /[,，、]/;

/** `"12"` → 12, `"true"` → true, anything else stays text. */
function coerceScalar(raw: string): unknown {
  const s = raw.trim();
  if (NUMBER_RE.test(s)) return Number(s);
  if (s === 'true') return true;
  if (s === 'false') return false;
  return s;
}

/** Parse one row's value for its operator; `undefined` + issue on bad input. */
export function parseConditionValue(
  op: ConditionOp,
  raw: string,
): { value: unknown } | { issue: 'conditionValue' | 'conditionNumber' } {
  const s = raw.trim();
  switch (op) {
    case 'gt':
    case 'gte':
    case 'lt':
    case 'lte': {
      if (!NUMBER_RE.test(s)) return { issue: 'conditionNumber' };
      return { value: Number(s) };
    }
    case 'in':
    case 'not_in': {
      const items = s
        .split(LIST_SPLIT_RE)
        .map((p) => p.trim())
        .filter((p) => p.length > 0);
      if (items.length === 0) return { issue: 'conditionValue' };
      return { value: items.map(coerceScalar) };
    }
    case 'contains':
      if (!s) return { issue: 'conditionValue' };
      return { value: s };
    default:
      if (!s) return { issue: 'conditionValue' };
      return { value: coerceScalar(s) };
  }
}

// ── Builders ────────────────────────────────────────────────

export function buildConditions(
  state: Pick<RuleFormState, 'match' | 'conditions'>,
): { conditions: AutopilotCondition; issues: BuildIssue[] } {
  const issues: BuildIssue[] = [];
  const leaves: AutopilotConditionLeaf[] = [];
  state.conditions.forEach((row, i) => {
    const field = row.field.trim();
    if (!field) {
      issues.push({ kind: 'conditionField', row: i });
      return;
    }
    if (row.typedValue !== undefined) {
      leaves.push({ field, op: row.op, value: row.typedValue });
      return;
    }
    const parsed = parseConditionValue(row.op, row.valueText);
    if ('issue' in parsed) {
      issues.push({ kind: parsed.issue, row: i });
      return;
    }
    leaves.push({ field, op: row.op, value: parsed.value });
  });
  const conditions: AutopilotCondition =
    state.match === 'any' ? { any: leaves } : { all: leaves };
  return { conditions, issues };
}

function trimmed(v: string): string {
  return v.trim();
}

/**
 * Build the action object. `base` (the saved action when editing) keeps any
 * key the form does not own — `screen`, `context_ticks`, … — verbatim.
 */
export function buildAction(
  state: RuleFormState,
  base?: AutopilotAction,
): { action: AutopilotAction; issues: BuildIssue[] } {
  const issues: BuildIssue[] = [];
  const rest: Record<string, unknown> = {};
  if (base) {
    for (const [k, v] of Object.entries(base)) {
      if (!FORM_OWNED_ACTION_KEYS.has(k)) rest[k] = v;
    }
  }
  const values: Record<string, string> = {
    target_agent: trimmed(state.targetAgent),
    prompt: trimmed(state.prompt),
    skill_name: trimmed(state.skillName),
    channel: trimmed(state.channel),
    chat_id: trimmed(state.chatId),
    text: trimmed(state.text),
  };
  const own: Record<string, string> = {};
  for (const key of ACTION_REQUIRED_FIELDS[state.actionType]) {
    if (!values[key]) issues.push({ kind: 'missing', field: key });
    own[key] = values[key];
  }
  return { action: { ...rest, type: state.actionType, ...own }, issues };
}

export function buildCreatePayload(
  state: RuleFormState,
): { payload: AutopilotCreateParams | null; issues: BuildIssue[] } {
  const issues: BuildIssue[] = [];
  if (!state.name.trim()) issues.push({ kind: 'missing', field: 'name' });
  const cond = buildConditions(state);
  const act = buildAction(state);
  issues.push(...cond.issues, ...act.issues);
  if (issues.length > 0) return { payload: null, issues };
  return {
    payload: {
      name: state.name.trim(),
      trigger_event: state.triggerEvent as AutopilotCreateTriggerEvent,
      conditions: cond.conditions,
      action: act.action,
    },
    issues,
  };
}

/** Key-order-independent JSON, for "did this change" checks. */
export function stableStringify(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(stableStringify).join(',')}]`;
  if (v && typeof v === 'object') {
    const entries = Object.keys(v as Record<string, unknown>)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stableStringify((v as Record<string, unknown>)[k])}`);
    return `{${entries.join(',')}}`;
  }
  return JSON.stringify(v ?? null);
}

/**
 * Build an `autopilot.update` field set holding only what changed. Fields the
 * form could not represent (locked conditions/action, sequence rules) are
 * never sent, so the server keeps them exactly as stored.
 */
export function buildUpdatePayload(
  state: RuleFormState,
  original: AutopilotRule,
): { fields: AutopilotUpdateFields | null; issues: BuildIssue[] } {
  const issues: BuildIssue[] = [];
  const fields: AutopilotUpdateFields = {};
  const name = state.name.trim();
  if (!name) issues.push({ kind: 'missing', field: 'name' });
  else if (name !== original.name) fields.name = name;

  if (!state.sequenceRule && state.triggerEvent !== original.trigger_event) {
    fields.trigger_event = state.triggerEvent;
  }

  if (!state.conditionsLocked && !state.sequenceRule && state.conditionsTouched) {
    const cond = buildConditions(state);
    issues.push(...cond.issues);
    if (stableStringify(cond.conditions) !== stableStringify(original.conditions)) {
      fields.conditions = cond.conditions;
    }
  }

  if (!state.actionLocked) {
    const act = buildAction(state, original.action);
    issues.push(...act.issues);
    if (stableStringify(act.action) !== stableStringify(original.action)) {
      fields.action = act.action;
    }
  }

  if (issues.length > 0) return { fields: null, issues };
  return { fields, issues };
}

// ── Loading a saved rule ────────────────────────────────────

function isScalar(v: unknown): v is string | number | boolean {
  return typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean';
}

function leafToRow(c: unknown): ConditionRow | null {
  if (!c || typeof c !== 'object' || Array.isArray(c)) return null;
  const obj = c as Record<string, unknown>;
  const keys = Object.keys(obj);
  if (keys.some((k) => k !== 'field' && k !== 'op' && k !== 'value')) return null;
  const field = obj.field;
  const op = (obj.op ?? 'eq') as string;
  if (typeof field !== 'string' || !field) return null;
  if (!(CONDITION_OPS as readonly string[]).includes(op)) return null;
  const value = obj.value;
  let valueText: string;
  if (isScalar(value)) valueText = String(value);
  else if (Array.isArray(value) && value.every(isScalar)) valueText = value.map(String).join(', ');
  else return null;
  return { field, op: op as ConditionOp, valueText, typedValue: value };
}

/** Saved conditions → flat rows, or `null` when they cannot be shown as such. */
export function conditionsToRows(
  conditions: AutopilotCondition | undefined,
): { match: 'all' | 'any'; rows: ConditionRow[] } | null {
  if (conditions == null) return { match: 'all', rows: [] };
  if (typeof conditions !== 'object' || Array.isArray(conditions)) return null;
  const obj = conditions as Record<string, unknown>;
  const keys = Object.keys(obj);
  // `{}` is the empty set: always matches, same as "no condition".
  if (keys.length === 0) return { match: 'all', rows: [] };
  for (const group of ['all', 'any'] as const) {
    if (keys.length === 1 && keys[0] === group) {
      const list = obj[group];
      if (!Array.isArray(list)) return null;
      const rows = list.map(leafToRow);
      if (rows.some((r) => r === null)) return null;
      return { match: group, rows: rows as ConditionRow[] };
    }
  }
  const single = leafToRow(obj);
  return single ? { match: 'all', rows: [single] } : null;
}

export function formStateFromRule(rule: AutopilotRule, defaultAgent = ''): RuleFormState {
  const base = emptyRuleForm(defaultAgent);
  const sequenceRule = rule.sequence != null;
  const parsed = conditionsToRows(rule.conditions);
  const action = rule.action ?? ({ type: '' } as AutopilotAction);
  const actionType = (FORM_ACTION_TYPES as readonly string[]).includes(action.type)
    ? (action.type as FormActionType)
    : null;
  const str = (v: unknown) => (typeof v === 'string' ? v : '');
  return {
    ...base,
    name: rule.name,
    triggerEvent: rule.trigger_event,
    match: parsed?.match ?? 'all',
    conditions: parsed?.rows ?? [],
    conditionsLocked: parsed === null,
    sequenceRule,
    actionLocked: actionType === null,
    actionType: actionType ?? base.actionType,
    targetAgent: str(action.target_agent) || str(action.agent_id) || base.targetAgent,
    prompt: str(action.prompt) || str(action.prompt_template),
    skillName: str(action.skill_name),
    channel: str(action.channel) || base.channel,
    chatId: str(action.chat_id),
    text: str(action.text),
  };
}

/** Short "who/where" label for a rule's action in the list. */
export function ruleActionTarget(action: AutopilotAction | undefined): string {
  if (!action) return '';
  const str = (v: unknown) => (typeof v === 'string' ? v : '');
  if (action.type === 'notify' || action.type === 'proactive_notify') {
    return str(action.channel);
  }
  return str(action.target_agent) || str(action.agent_id);
}
