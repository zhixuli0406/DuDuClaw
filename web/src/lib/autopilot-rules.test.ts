import { describe, it, expect } from 'vitest';
import {
  CONDITION_OPS,
  FORM_ACTION_TYPES,
  FORM_TRIGGER_EVENTS,
  LEGACY_TRIGGER_EVENTS,
  SERVER_TRIGGER_EVENTS,
  buildCreatePayload,
  buildUpdatePayload,
  conditionsToRows,
  emptyRuleForm,
  formStateFromRule,
  parseConditionValue,
  type AutopilotRule,
  type FormActionType,
  type RuleFormState,
} from './autopilot-rules';

// ── A TypeScript replica of the gateway's write-time checks ──────────────
// `validate_autopilot_trigger_event` + `validate_autopilot_action`
// (crates/duduclaw-gateway/src/handlers/task_json.rs) and the condition
// shapes `autopilot_engine::evaluate` understands. If these drift from the
// Rust side, update both together.
// AUTOPILOT_CREATE_TRIGGER_EVENTS / AUTOPILOT_LEGACY_TRIGGER_EVENTS.
const SERVER_CREATE_TRIGGERS = [
  'task_created', 'task_updated', 'task_status_changed', 'activity_new',
  'channel_message', 'agent_idle', 'run_at_risk', 'os_file',
  'os_frontmost', 'tick', 'security_event', 'odoo_event',
];
const SERVER_LEGACY_TRIGGERS = ['cron_tick'];
const SERVER_KNOWN_TRIGGERS = [...SERVER_CREATE_TRIGGERS, ...SERVER_LEGACY_TRIGGERS];
const SERVER_REQUIRED: Record<string, string[]> = {
  delegate: ['target_agent', 'prompt'],
  notify: ['channel', 'chat_id', 'text'],
  run_skill: ['target_agent', 'skill_name'],
  proactive_notify: ['channel', 'chat_id', 'text'],
};
const ENGINE_OPS = ['eq', 'neq', 'in', 'not_in', 'gt', 'gte', 'lt', 'lte', 'contains'];

function serverAccepts(payload: Record<string, unknown>): string | null {
  if (typeof payload.name !== 'string' || !payload.name) return 'name is required';
  const ev = payload.trigger_event as string;
  if (!SERVER_KNOWN_TRIGGERS.includes(ev)) return `unknown trigger_event '${ev}'`;
  const action = payload.action as Record<string, unknown>;
  if (!action || typeof action !== 'object') return 'action must be a JSON object';
  const req = SERVER_REQUIRED[action.type as string];
  if (!req) return `unknown action.type '${String(action.type)}'`;
  for (const k of req) {
    if (typeof action[k] !== 'string' || !(action[k] as string)) {
      return `action.${k} is required for type '${String(action.type)}'`;
    }
  }
  return null;
}

/** True when the engine would ever evaluate this condition to something useful. */
function engineShapeOk(c: unknown): boolean {
  if (c === null) return true;
  if (typeof c !== 'object' || Array.isArray(c)) return false;
  const o = c as Record<string, unknown>;
  if (Array.isArray(o.all)) return o.all.every(engineShapeOk);
  if (Array.isArray(o.any)) return o.any.every(engineShapeOk);
  // A bare `{}` never matches in the engine.
  return typeof o.field === 'string' && o.field.length > 0 && ENGINE_OPS.includes(o.op as string);
}

function filledForm(trigger: string, actionType: FormActionType): RuleFormState {
  return {
    ...emptyRuleForm('sales-lead'),
    name: `rule ${trigger} ${actionType}`,
    triggerEvent: trigger,
    actionType,
    prompt: 'Handle {task.title}',
    skillName: 'weekly-report',
    channel: 'line',
    chatId: 'C1234567890',
    text: 'Heads up: {text}',
  };
}

describe('mirrored server constants', () => {
  it('trigger list matches the gateway create list exactly; cron_tick is legacy only', () => {
    expect([...SERVER_TRIGGER_EVENTS]).toEqual(SERVER_CREATE_TRIGGERS);
    expect([...LEGACY_TRIGGER_EVENTS]).toEqual(SERVER_LEGACY_TRIGGERS);
  });
  it('condition operators match autopilot_engine::CONDITION_OPS', () => {
    expect([...CONDITION_OPS]).toEqual(ENGINE_OPS);
  });
  it('the form never offers schedule or the never-emitted cron_tick', () => {
    expect(FORM_TRIGGER_EVENTS).not.toContain('schedule');
    expect(FORM_TRIGGER_EVENTS).not.toContain('cron_tick');
    for (const ev of FORM_TRIGGER_EVENTS) expect(SERVER_KNOWN_TRIGGERS).toContain(ev);
  });
});

describe('buildCreatePayload — every offered trigger × action', () => {
  for (const trigger of FORM_TRIGGER_EVENTS) {
    for (const actionType of FORM_ACTION_TYPES) {
      it(`${trigger} → ${actionType} is accepted by the server`, () => {
        const { payload, issues } = buildCreatePayload(filledForm(trigger, actionType));
        expect(issues).toEqual([]);
        expect(payload).not.toBeNull();
        expect(serverAccepts(payload as unknown as Record<string, unknown>)).toBeNull();
        expect(engineShapeOk(payload!.conditions)).toBe(true);
        // Never the legacy keys the server ignored.
        expect(payload!.action).not.toHaveProperty('agent_id');
        expect(payload!.action).not.toHaveProperty('prompt_template');
      });
    }
  }

  it('produces the exact delegate shape the team templates use', () => {
    const { payload } = buildCreatePayload(filledForm('task_created', 'delegate'));
    expect(payload).toEqual({
      name: 'rule task_created delegate',
      trigger_event: 'task_created',
      conditions: { all: [] },
      action: { type: 'delegate', target_agent: 'sales-lead', prompt: 'Handle {task.title}' },
    });
  });

  it('produces the exact notify shape', () => {
    const { payload } = buildCreatePayload(filledForm('channel_message', 'notify'));
    expect(payload?.action).toEqual({
      type: 'notify', channel: 'line', chat_id: 'C1234567890', text: 'Heads up: {text}',
    });
  });

  it('produces the exact run_skill shape', () => {
    const { payload } = buildCreatePayload(filledForm('agent_idle', 'run_skill'));
    expect(payload?.action).toEqual({
      type: 'run_skill', target_agent: 'sales-lead', skill_name: 'weekly-report',
    });
  });

  it('expresses a resident-sensing condition on a free-form field', () => {
    const form: RuleFormState = {
      ...filledForm('tick', 'delegate'),
      conditions: [
        { field: 'source', op: 'eq', valueText: 'twse-2330' },
        { field: 'pct_price', op: 'gt', valueText: '2' },
      ],
    };
    const { payload } = buildCreatePayload(form);
    expect(payload?.conditions).toEqual({
      all: [
        { field: 'source', op: 'eq', value: 'twse-2330' },
        { field: 'pct_price', op: 'gt', value: 2 },
      ],
    });
  });

  it('expresses an Odoo condition with any-match and a list', () => {
    const form: RuleFormState = {
      ...filledForm('odoo_event', 'notify'),
      match: 'any',
      conditions: [
        { field: 'event_type', op: 'in', valueText: 'odoo.sale.order_confirmed, odoo.crm.lead_created' },
        { field: 'stage_id', op: 'eq', valueText: '3' },
      ],
    };
    const { payload } = buildCreatePayload(form);
    expect(payload?.conditions).toEqual({
      any: [
        { field: 'event_type', op: 'in', value: ['odoo.sale.order_confirmed', 'odoo.crm.lead_created'] },
        { field: 'stage_id', op: 'eq', value: 3 },
      ],
    });
  });

  it('reports missing required fields instead of sending', () => {
    const form: RuleFormState = { ...filledForm('task_created', 'notify'), chatId: '  ', name: '' };
    const { payload, issues } = buildCreatePayload(form);
    expect(payload).toBeNull();
    expect(issues).toEqual([
      { kind: 'missing', field: 'name' },
      { kind: 'missing', field: 'chat_id' },
    ]);
  });

  it('reports bad condition rows', () => {
    const form: RuleFormState = {
      ...filledForm('tick', 'delegate'),
      conditions: [
        { field: '', op: 'eq', valueText: 'x' },
        { field: 'pct_price', op: 'gt', valueText: 'two' },
        { field: 'source', op: 'eq', valueText: '' },
      ],
    };
    expect(buildCreatePayload(form).issues).toEqual([
      { kind: 'conditionField', row: 0 },
      { kind: 'conditionNumber', row: 1 },
      { kind: 'conditionValue', row: 2 },
    ]);
  });
});

describe('parseConditionValue', () => {
  it('coerces numbers and booleans for equality, keeps text for contains', () => {
    expect(parseConditionValue('eq', '42')).toEqual({ value: 42 });
    expect(parseConditionValue('eq', 'true')).toEqual({ value: true });
    expect(parseConditionValue('eq', 'high')).toEqual({ value: 'high' });
    expect(parseConditionValue('contains', '42')).toEqual({ value: '42' });
    expect(parseConditionValue('not_in', 'a，b、c')).toEqual({ value: ['a', 'b', 'c'] });
    expect(parseConditionValue('lte', '-1.5')).toEqual({ value: -1.5 });
  });
});

// A rule exactly as the pharmacy team template / OS page creates it, plus the
// optional extras a hand-written rule may carry (`screen`, `context_ticks`).
const TEMPLATE_RULE: AutopilotRule = {
  id: 'r1',
  name: 'pharmacy-adverse-reaction-notify',
  enabled: true,
  trigger_event: 'channel_message',
  conditions: {
    any: [
      { field: 'text', op: 'contains', value: '呼吸困難' },
      { field: 'text', op: 'contains', value: '意識改變' },
    ],
  },
  action: {
    type: 'notify',
    channel: 'line',
    chat_id: 'C999',
    text: '疑似嚴重藥品不良反應',
    screen: { mode: 'local', prompt: 'is it urgent?' },
  },
  created_at: '2026-10-01T00:00:00Z',
  trigger_count: 4,
  sequence: null,
  metadata: null,
};

describe('round-trip of a template-created rule', () => {
  it('loads into the form and saves back with nothing changed', () => {
    const state = formStateFromRule(TEMPLATE_RULE);
    expect(state.conditionsLocked).toBe(false);
    expect(state.actionLocked).toBe(false);
    expect(state.match).toBe('any');
    expect(state.conditions.map((c) => c.valueText)).toEqual(['呼吸困難', '意識改變']);
    const { fields, issues } = buildUpdatePayload(state, TEMPLATE_RULE);
    expect(issues).toEqual([]);
    expect(fields).toEqual({});
  });

  it('editing the text keeps unknown-but-valid action keys', () => {
    const state = { ...formStateFromRule(TEMPLATE_RULE), text: '請藥師立即接手' };
    const { fields } = buildUpdatePayload(state, TEMPLATE_RULE);
    expect(fields).toEqual({
      action: {
        type: 'notify',
        channel: 'line',
        chat_id: 'C999',
        text: '請藥師立即接手',
        screen: { mode: 'local', prompt: 'is it urgent?' },
      },
    });
    expect(serverAccepts({ ...TEMPLATE_RULE, ...fields })).toBeNull();
  });

  it('an untouched string value that looks numeric stays a string', () => {
    const rule: AutopilotRule = {
      ...TEMPLATE_RULE,
      conditions: { all: [{ field: 'agent_id', op: 'eq', value: '007' }] },
    };
    const state = { ...formStateFromRule(rule), conditionsTouched: true, match: 'all' as const };
    const { fields } = buildUpdatePayload(state, rule);
    // Same conditions → nothing to send.
    expect(fields).toEqual({});
  });

  it('nested or unknown condition shapes are locked and never re-sent', () => {
    const rule: AutopilotRule = {
      ...TEMPLATE_RULE,
      conditions: { all: [{ any: [{ field: 'a', op: 'eq', value: 1 }] }] },
    };
    const state = formStateFromRule(rule);
    expect(state.conditionsLocked).toBe(true);
    const { fields } = buildUpdatePayload({ ...state, name: 'renamed', conditionsTouched: true }, rule);
    expect(fields).toEqual({ name: 'renamed' });
  });

  it('an empty set ({} / null) is "no condition" and stays editable', () => {
    expect(conditionsToRows({})).toEqual({ match: 'all', rows: [] });
    expect(conditionsToRows(null)).toEqual({ match: 'all', rows: [] });
    const rule: AutopilotRule = { ...TEMPLATE_RULE, conditions: {} };
    const state = formStateFromRule(rule);
    expect(state.conditionsLocked).toBe(false);
    // Untouched → not re-sent.
    expect(buildUpdatePayload(state, rule).fields).toEqual({});
    // Adding a condition sends it.
    const edited: RuleFormState = {
      ...state,
      conditionsTouched: true,
      conditions: [{ field: 'text', op: 'contains', valueText: 'refund' }],
    };
    expect(buildUpdatePayload(edited, rule).fields).toEqual({
      conditions: { all: [{ field: 'text', op: 'contains', value: 'refund' }] },
    });
  });

  it('a stored cron_tick rule keeps its trigger when edited', () => {
    const rule: AutopilotRule = { ...TEMPLATE_RULE, trigger_event: 'cron_tick' };
    const state = { ...formStateFromRule(rule), name: 'renamed' };
    expect(buildUpdatePayload(state, rule).fields).toEqual({ name: 'renamed' });
  });

  it('sequence rules keep trigger and conditions; proactive_notify keeps its action', () => {
    const rule: AutopilotRule = {
      ...TEMPLATE_RULE,
      trigger_event: 'task_created',
      sequence: { first: { event: 'task_created' }, then: { event: 'task_status_changed' }, within_secs: 60 },
      action: { type: 'proactive_notify', channel: 'line', chat_id: 'C1', text: 'x' },
    };
    const state = formStateFromRule(rule);
    expect(state.sequenceRule).toBe(true);
    expect(state.actionLocked).toBe(true);
    const { fields } = buildUpdatePayload(
      { ...state, triggerEvent: 'tick', conditionsTouched: true, name: 'n2' },
      rule,
    );
    expect(fields).toEqual({ name: 'n2' });
  });

  it('switching action type drops the old type\'s fields but keeps extras', () => {
    const state: RuleFormState = {
      ...formStateFromRule(TEMPLATE_RULE),
      actionType: 'delegate',
      targetAgent: 'pharmacy-care',
      prompt: 'Follow up: {text}',
    };
    const { fields } = buildUpdatePayload(state, TEMPLATE_RULE);
    expect(fields?.action).toEqual({
      type: 'delegate',
      target_agent: 'pharmacy-care',
      prompt: 'Follow up: {text}',
      screen: { mode: 'local', prompt: 'is it urgent?' },
    });
  });
});
