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
  active_window?: unknown;
  run_policy?: unknown;
  owner_department?: string | null;
  created_by_user_id?: string | null;
  updated_by_user_id?: string | null;
  updated_at?: string | null;
  creator_access_lost?: boolean | null;
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
  'require_approval',
  'minutes',
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
  /** Unknown actions or step types stay locked and are never re-sent. */
  actionLocked: boolean;
  /** Event-sequence rules: trigger and conditions are not the form's to change. */
  sequenceRule: boolean;
  steps: StepDraft[];
  /** Preserve the existing envelope and extensions even with a single step. */
  stepsEnvelope: boolean;
  screen?: unknown;
  active_window?: unknown;
  run_policy?: unknown;
}

export interface StepDraft {
  actionType: FormActionType | 'wait';
  targetAgent: string;
  prompt: string;
  skillName: string;
  channel: string;
  chatId: string;
  text: string;
  minutes: number;
  requireApproval: boolean;
  /** Unowned fields travel with the step when reordered. */
  original?: AutopilotAction;
}

export function emptyStep(actionType: StepDraft['actionType'] = 'delegate', defaultAgent = ''): StepDraft {
  return {
    actionType, targetAgent: defaultAgent, prompt: '', skillName: '',
    channel: 'telegram', chatId: '', text: '', minutes: 1, requireApproval: false,
  };
}

export function emptyRuleForm(defaultAgent = ''): RuleFormState {
  return {
    name: '', triggerEvent: 'task_created', match: 'all', conditions: [],
    conditionsLocked: false, conditionsTouched: false, actionLocked: false,
    sequenceRule: false, steps: [emptyStep('delegate', defaultAgent)], stepsEnvelope: false,
  };
}

// ── Value parsing ───────────────────────────────────────────

export type BuildIssue =
  | { kind: 'missing'; field: string; step?: number }
  | { kind: 'stepCount' | 'waitDuration' | 'waitOrder'; steps: number[] }
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

/** Build steps without moving per-step approval or extras onto the envelope. */
export function buildAction(
  state: RuleFormState,
): { action: AutopilotAction; issues: BuildIssue[] } {
  const issues: BuildIssue[] = [];
  const drafts = state.steps;
  if (drafts.length < 1 || drafts.length > 5) {
    issues.push({ kind: 'stepCount', steps: drafts.map((_, i) => i).filter(i => i >= 5) });
  }
  const waits = drafts.flatMap((s, i) => s.actionType === 'wait' ? [i] : []);
  const invalidWaits = waits.filter(i => !Number.isInteger(drafts[i].minutes) || drafts[i].minutes < 1 || drafts[i].minutes > 1440);
  if (waits.reduce((sum, i) => sum + drafts[i].minutes, 0) > 1440) invalidWaits.push(...waits);
  if (invalidWaits.length) issues.push({ kind: 'waitDuration', steps: [...new Set(invalidWaits)] });
  const invalidOrder = waits.filter(i => i === drafts.length - 1 || drafts[i + 1]?.actionType === 'wait' || drafts[i - 1]?.actionType === 'wait');
  if (invalidOrder.length) issues.push({ kind: 'waitOrder', steps: invalidOrder });

  const steps = drafts.map((step, index): AutopilotAction => {
    if (step.actionType === 'wait') return { type: 'wait', minutes: step.minutes };
    const rest = Object.fromEntries(Object.entries(step.original ?? {}).filter(([key]) => !FORM_OWNED_ACTION_KEYS.has(key)));
    const values: Record<string, string> = {
      target_agent: step.targetAgent, prompt: step.prompt, skill_name: step.skillName,
      channel: step.channel, chat_id: step.chatId, text: step.text,
    };
    const action: AutopilotAction = { ...rest, type: step.actionType };
    for (const key of ACTION_REQUIRED_FIELDS[step.actionType]) {
      // Keep untouched wire strings exact, including whitespace.
      action[key] = step.original?.[key] === values[key] ? values[key] : values[key].trim();
      if (!values[key].trim()) issues.push({ kind: 'missing', field: key, step: index });
    }
    if (step.requireApproval || step.original?.require_approval === false) action.require_approval = step.requireApproval;
    return action;
  });
  const useEnvelope = state.stepsEnvelope || steps.length !== 1 || steps[0]?.type === 'wait'
    || drafts.some(s => s.requireApproval || s.original?.require_approval !== undefined || s.original?.params != null)
    || state.active_window != null || state.run_policy != null;
  if (!useEnvelope) return { action: steps[0], issues };
  // screen belongs to the envelope, never copied to its single legacy step.
  if (!state.stepsEnvelope) for (const step of steps) delete step.screen;
  return {
    action: { type: 'steps', steps, ...(state.screen !== undefined ? { screen: state.screen } : {}) },
    issues,
  };
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
    const act = buildAction(state);
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
  const wireSteps = action.type === 'steps' ? action.steps : [action];
  const editable = Array.isArray(wireSteps) && wireSteps.every(step => step && typeof step === 'object'
    && [...FORM_ACTION_TYPES, 'wait'].includes(step.type));
  const str = (v: unknown) => (typeof v === 'string' ? v : '');
  const steps: StepDraft[] = editable ? (wireSteps as AutopilotAction[]).map(step => ({
    ...emptyStep(step.type as StepDraft['actionType'], defaultAgent),
    targetAgent: str(step.target_agent) || str(step.agent_id) || defaultAgent,
    prompt: str(step.prompt) || str(step.prompt_template), skillName: str(step.skill_name),
    channel: str(step.channel) || 'telegram', chatId: str(step.chat_id), text: str(step.text),
    minutes: typeof step.minutes === 'number' ? step.minutes : 1,
    requireApproval: step.require_approval === true, original: step,
  })) : base.steps;
  return {
    ...base,
    name: rule.name,
    triggerEvent: rule.trigger_event,
    match: parsed?.match ?? 'all',
    conditions: parsed?.rows ?? [],
    conditionsLocked: parsed === null,
    sequenceRule,
    actionLocked: !editable,
    steps,
    stepsEnvelope: action.type === 'steps',
    screen: action.screen,
    active_window: rule.active_window,
    run_policy: rule.run_policy,
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
