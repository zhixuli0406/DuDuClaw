/**
 * L7: the readable half of a `discovery` approval — the frozen run spec at
 * `payload.request.spec`. The payload is untrusted DATA: every field is
 * type-checked and dropped when wrong, strings are length-capped and only ever
 * rendered as React text. `null` ⇒ nothing usable, caller falls back to the
 * generic view (the raw spot-check expander always stays).
 */
export interface DiscoveryApprovalSummary {
  runtime?: string;
  model?: string;
  evaluator?: string;
  branchCount?: number;
  refineCount?: number;
  maxRounds?: number;
  maxAgentCalls?: number;
  maxUsd?: number;
  maxWallSecs?: number;
}

const MAX_TEXT_CHARS = 120;

function obj(v: unknown): Record<string, unknown> | null {
  return v != null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

function text(v: unknown): string | undefined {
  if (typeof v !== 'string' || v.trim() === '') return undefined;
  const chars = Array.from(v); // codepoint-safe cap
  return chars.length > MAX_TEXT_CHARS ? `${chars.slice(0, MAX_TEXT_CHARS).join('')}…` : v;
}

function count(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) && v >= 0 ? v : undefined;
}

export function parseDiscoveryApproval(payload: unknown): DiscoveryApprovalSummary | null {
  const spec = obj(obj(obj(payload)?.request)?.spec);
  if (!spec) return null;
  const budget = obj(spec.budget);
  const raw: DiscoveryApprovalSummary = {
    runtime: text(spec.runtime),
    model: text(spec.model),
    evaluator: text(spec.evaluator),
    branchCount: count(spec.branch_count),
    refineCount: count(spec.refine_count),
    maxRounds: count(budget?.max_rounds),
    maxAgentCalls: count(budget?.max_agent_calls),
    maxUsd: count(budget?.max_usd),
    maxWallSecs: count(budget?.max_wall_secs),
  };
  const summary = Object.fromEntries(
    Object.entries(raw).filter(([, v]) => v !== undefined),
  ) as DiscoveryApprovalSummary;
  return Object.keys(summary).length > 0 ? summary : null;
}
