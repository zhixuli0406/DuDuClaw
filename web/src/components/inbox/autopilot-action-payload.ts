/**
 * v1.82 (H1): the frozen action of an `autopilot_action` approval card —
 * exactly what runs once an Admin approves (`autopilot_approval.rs`
 * `FrozenDispatch::approval_payload`). Every string is kept COMPLETE: no
 * truncation anywhere (a benign prefix could hide what follows). The payload
 * is untrusted DATA: a field of the wrong type makes the whole card
 * unrenderable (`null`), and the view then refuses to offer Approve.
 */
export type AutopilotActionType = 'delegate' | 'run_skill' | 'notify' | 'proactive_notify';

export interface AutopilotActionCard {
  dispatchId: string;
  ruleId: string;
  ruleName: string;
  /** Full rule revision digest (never shortened). */
  ruleHash: string;
  event: string;
  eventRef: string;
  sourceAgent?: string;
  actionType: AutopilotActionType;
  targetAgent?: string;
  skillName?: string;
  lane?: string;
  channel?: string;
  chatId?: string;
  /** The complete prompt (skill body included for `run_skill`) or message. */
  content: string;
  frozenDigest: string;
  latestExecutionAt?: string;
  executionWindowSecs?: number;
}

/** Same hard limit the gateway applies when filing the card. */
export const MAX_FROZEN_PAYLOAD_BYTES = 64 * 1024;

function obj(v: unknown): Record<string, unknown> | null {
  return v != null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

function str(v: unknown): string | null {
  return typeof v === 'string' ? v : null;
}

function optStr(v: unknown): string | undefined | null {
  if (v === undefined || v === null) return undefined;
  return typeof v === 'string' ? v : null;
}

/** Parse the card payload, or `null` when it cannot be shown completely. */
export function parseAutopilotAction(payload: unknown): AutopilotActionCard | null {
  let raw: unknown = payload;
  if (typeof raw === 'string') {
    try {
      raw = JSON.parse(raw);
    } catch {
      return null;
    }
  }
  const root = obj(raw);
  const dispatch = obj(root?.autopilot_dispatch);
  const action = obj(dispatch?.action);
  if (!root || !dispatch || !action) return null;
  const actionType = str(action.type);
  if (actionType !== 'delegate' && actionType !== 'run_skill' && actionType !== 'notify' && actionType !== 'proactive_notify') {
    return null;
  }
  const required = {
    dispatchId: str(dispatch.dispatch_id),
    ruleId: str(dispatch.rule_id),
    ruleName: str(dispatch.rule_name),
    ruleHash: str(dispatch.rule_hash),
    event: str(dispatch.event),
    eventRef: str(dispatch.event_ref),
    frozenDigest: str(root.frozen_digest),
  };
  if (Object.values(required).some((v) => v === null)) return null;
  const sourceAgent = optStr(dispatch.source_agent);
  const latestExecutionAt = optStr(root.latest_execution_at);
  const window = root.execution_window_secs;
  if (sourceAgent === null || latestExecutionAt === null) return null;
  const base = {
    dispatchId: required.dispatchId as string,
    ruleId: required.ruleId as string,
    ruleName: required.ruleName as string,
    ruleHash: required.ruleHash as string,
    event: required.event as string,
    eventRef: required.eventRef as string,
    frozenDigest: required.frozenDigest as string,
    sourceAgent,
    latestExecutionAt,
    executionWindowSecs: typeof window === 'number' && Number.isFinite(window) ? window : undefined,
  };
  if (actionType === 'delegate' || actionType === 'run_skill') {
    const targetAgent = str(action.target_agent);
    const prompt = str(action.prompt);
    const lane = optStr(action.lane);
    const skillName = actionType === 'run_skill' ? str(action.skill_name) : undefined;
    if (targetAgent === null || prompt === null || lane === null || skillName === null) return null;
    return { ...base, actionType, targetAgent, skillName, lane, content: prompt };
  }
  const channel = str(action.channel);
  const chatId = str(action.chat_id);
  const text = str(action.text);
  if (channel === null || chatId === null || text === null) return null;
  return { ...base, actionType, channel, chatId, content: text };
}

/** UTF-8 byte length of the serialized payload (the gateway's limit unit). */
export function payloadBytes(payload: unknown): number {
  const text = typeof payload === 'string' ? payload : JSON.stringify(payload ?? null);
  return new TextEncoder().encode(text).length;
}
