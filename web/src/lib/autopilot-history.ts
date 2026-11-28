/**
 * Every `result` the gateway writes to `autopilot_history` (the autopilot
 * settings tab's "Trigger History" dialog), mirrored from the Rust sources:
 *
 *  - `autopilot_approval.rs` `RESULT_*` (v1.82 approval gate, including the
 *    pre-v1.82 `success` / `failure` / `skipped`);
 *  - `autopilot_engine.rs` circuit-breaker transitions (`circuit_open`,
 *    `circuit_half_open`, `circuit_closed`);
 *  - `autopilot_screen.rs` `RESULT_SCREEN_DROP` / `RESULT_SCREEN_UNAVAILABLE`;
 *  - `autopilot_notify.rs` (`paused_by_human`, the chat "pause this rule" button).
 *
 * A value not listed here (a newer gateway) still renders: its raw status in
 * a neutral tone — never a crash and never a missing-message id.
 */
export const AUTOPILOT_HISTORY_RESULTS = [
  'success',
  'failure',
  'skipped',
  'awaiting_approval',
  'blocked_no_approval_service',
  'approval_error',
  'denied',
  'expired',
  'stale_after_approval',
  'execution_uncertain',
  'blocked_breaker_open',
  'circuit_open',
  'circuit_half_open',
  'circuit_closed',
  'screen_drop',
  'screen_unavailable',
  'paused_by_human',
] as const;

export type AutopilotHistoryResult = (typeof AUTOPILOT_HISTORY_RESULTS)[number];

export type AutopilotHistoryTone = 'success' | 'neutral' | 'warning' | 'error';

const TONE: Record<AutopilotHistoryResult, AutopilotHistoryTone> = {
  success: 'success',
  circuit_closed: 'success',
  skipped: 'neutral',
  denied: 'neutral',
  screen_drop: 'neutral',
  paused_by_human: 'neutral',
  awaiting_approval: 'warning',
  expired: 'warning',
  stale_after_approval: 'warning',
  circuit_half_open: 'warning',
  screen_unavailable: 'warning',
  failure: 'error',
  approval_error: 'error',
  blocked_no_approval_service: 'error',
  blocked_breaker_open: 'error',
  execution_uncertain: 'error',
  circuit_open: 'error',
};

const KNOWN: ReadonlySet<string> = new Set(AUTOPILOT_HISTORY_RESULTS);

export function isKnownHistoryResult(result: unknown): result is AutopilotHistoryResult {
  return typeof result === 'string' && KNOWN.has(result);
}

/** i18n key of a known status (`autopilot.history.<status>`). */
export function historyResultMessageId(result: AutopilotHistoryResult): string {
  return `autopilot.history.${result}`;
}

export interface HistoryResultDisplay {
  /** Set for a known status; `null` ⇒ render `raw` verbatim. */
  messageId: string | null;
  raw: string;
  tone: AutopilotHistoryTone;
}

/** How one history row's status renders. Unknown ⇒ raw text, neutral tone. */
export function describeHistoryResult(result: unknown): HistoryResultDisplay {
  if (isKnownHistoryResult(result)) {
    return { messageId: historyResultMessageId(result), raw: result, tone: TONE[result] };
  }
  return { messageId: null, raw: typeof result === 'string' ? result : String(result ?? ''), tone: 'neutral' };
}

export const HISTORY_TONE_CLASS: Record<AutopilotHistoryTone, string> = {
  success: 'bg-success/10 text-success',
  neutral: 'bg-muted text-muted-foreground',
  warning: 'bg-warning/10 text-warning',
  error: 'bg-destructive/10 text-destructive',
};


export interface AutopilotRunStep {
  step_index: number;
  step_type: string;
  state: string;
  started_at: string | null;
  finished_at: string | null;
  detail: string | null;
}
export interface AutopilotRun {
  run_id: string;
  state: string;
  step_index: number;
  step_count: number;
  next_at: string | null;
  outcome: string | null;
  detail: string | null;
  event_name: string;
  created_at: string;
  updated_at: string;
  steps: AutopilotRunStep[];
}
const RUN_STATES = new Set(['pending', 'waiting', 'awaiting_approval', 'running', 'completed', 'done', 'failed', 'skipped', 'expired', 'denied', 'settled']);
const RUN_REASONS = new Set(['outside_window', 'outside_window_tz_unresolved', 'stale_rule_changed', 'rule_disabled', 'rule_removed', 'missed_late', 'param_injection_blocked', 'scope_denied', 'cancelled_by_admin', 'missing_field']);
export function runStateMessageId(state: string): string {
  return `autopilot.runs.state.${RUN_STATES.has(state) ? state : 'unknown'}`;
}
/** Never expose untrusted details or internal codes, including parameter suffixes. */
export function runReasonMessageId(code: string): string {
  if (code === 'completed' || code === 'done') return runStateMessageId(code);
  const base = code.split(':')[0];
  const reason = base === 'param_missing' ? 'missing_field' : base;
  return `autopilot.runs.reason.${RUN_REASONS.has(reason) ? reason : 'unknown'}`;
}
export function canCancelRun(state: string): boolean {
  return ['pending', 'waiting', 'awaiting_approval'].includes(state);
}
