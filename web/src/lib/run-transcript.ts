/**
 * Run transcript transforms (G12 run inspector) — pure functions that shape
 * the `runs.get` event stream into renderable transcript cards.
 *
 * Honesty contract (mirrors the gateway): the persisted per-run record is
 * session text turns + MCP tool-call receipts + (since the run_steps.db
 * upgrade) CLI-native tool steps and TodoWrite board snapshots. Thinking
 * summaries are still live-only — the gateway reports them under
 * `not_persisted` and the UI states that instead of faking events.
 */

export type RunStatus = 'completed' | 'running' | 'no_reply';

export interface RunSummary {
  id: string;
  session_id: string;
  agent_id: string;
  /** Session channel prefix ("telegram", "discord", …) or "other". */
  channel: string;
  /** RFC3339. */
  started_at: string;
  /** RFC3339; null while running / never answered. */
  ended_at: string | null;
  status: RunStatus;
  /** MCP tool receipts correlated to this run's window. */
  step_count: number;
  /** First chars of the user turn. */
  preview: string;
}

/** One raw event from `runs.get` (kinds beyond these may appear in future
 *  gateways — unknown kinds are skipped defensively, never mis-rendered). */
export interface RunEvent {
  kind: string;
  /** For kind "text". */
  role?: 'user' | 'assistant';
  /** For kind "tool_use". */
  tool?: string;
  ok?: boolean;
  /** For kinds "tool_step" (tool name) / "todo_update" ("done/total"). */
  label?: string;
  /** For run_steps.db kinds: per-run monotonic ordering hint. */
  seq?: number;
  ts: string;
  preview: string;
}

export interface RunDetail {
  run: Omit<RunSummary, 'step_count' | 'preview'>;
  events: RunEvent[];
  event_sources?: Record<string, string>;
  /** Machine keys of live-only event kinds that are not persisted. */
  not_persisted?: string[];
}

/** Renderable transcript card — a prose block, a collapsible tool card, a
 *  CLI-native tool step, or a TodoWrite board snapshot. */
export type TranscriptCard =
  | { type: 'prose'; role: 'user' | 'assistant'; ts: string; text: string }
  | { type: 'tool'; tool: string; ok: boolean; ts: string; preview: string }
  | { type: 'step'; tool: string; ts: string; preview: string }
  | { type: 'todo'; label: string; ts: string; preview: string };

/**
 * Shape raw events into transcript cards. Defensive: events with unknown
 * kinds or missing required fields are dropped (an incomplete record must
 * not render as a fabricated one); ordering is preserved as delivered
 * (the gateway already sorts chronologically).
 */
export function cardsForEvents(events: readonly RunEvent[]): TranscriptCard[] {
  const cards: TranscriptCard[] = [];
  for (const e of events) {
    if (e.kind === 'text' && (e.role === 'user' || e.role === 'assistant')) {
      cards.push({ type: 'prose', role: e.role, ts: e.ts, text: e.preview });
    } else if (e.kind === 'tool_use' && typeof e.tool === 'string' && e.tool.length > 0) {
      cards.push({
        type: 'tool',
        tool: e.tool,
        ok: e.ok !== false,
        ts: e.ts,
        preview: e.preview ?? '',
      });
    } else if (e.kind === 'tool_step' && typeof e.label === 'string' && e.label.length > 0) {
      // CLI-native step boundary from run_steps.db — label is the tool name;
      // no ok flag is persisted, so none is rendered (never invented).
      cards.push({ type: 'step', tool: e.label, ts: e.ts, preview: e.preview ?? '' });
    } else if (e.kind === 'todo_update' && typeof e.label === 'string' && e.label.length > 0) {
      // TodoWrite board snapshot — label is "done/total", preview the
      // already-rendered board text.
      cards.push({ type: 'todo', label: e.label, ts: e.ts, preview: e.preview ?? '' });
    }
    // Unknown kinds: skipped on purpose.
  }
  return cards;
}

/** A run is live while the gateway reports it as still being answered. */
export function isRunLive(run: Pick<RunSummary, 'status'>): boolean {
  return run.status === 'running';
}

/** Status → badge metadata (i18n id + status token). Tokens only — the CSS
 *  vars come from the Soft Play status system, no hex here. */
export function runStatusMeta(status: RunStatus | string): {
  labelId: string;
  token: string;
} {
  switch (status) {
    case 'running':
      return { labelId: 'runs.status.running', token: 'var(--status-agent-running)' };
    case 'completed':
      return { labelId: 'runs.status.completed', token: 'var(--status-task-done)' };
    default:
      return { labelId: 'runs.status.no_reply', token: 'var(--status-agent-idle)' };
  }
}

/** Compact relative-time label parts, i18n-friendly: the caller feeds the
 *  value/unit into `intl.formatRelativeTime`. Falls back to null when the
 *  timestamp doesn't parse (caller shows the raw string instead). */
export function relativeParts(
  ts: string,
  nowMs: number,
): { value: number; unit: 'minute' | 'hour' | 'day' } | null {
  const ms = Date.parse(ts);
  if (!Number.isFinite(ms)) return null;
  const diffMin = Math.round((ms - nowMs) / 60_000);
  if (Math.abs(diffMin) < 60) return { value: diffMin, unit: 'minute' };
  const diffH = Math.round(diffMin / 60);
  if (Math.abs(diffH) < 24) return { value: diffH, unit: 'hour' };
  return { value: Math.round(diffH / 24), unit: 'day' };
}

/** Wall-clock duration of a finished run, in whole seconds (null while the
 *  run has no real end — durations are never invented). */
export function runDurationSecs(run: Pick<RunSummary, 'started_at' | 'ended_at'>): number | null {
  if (!run.ended_at) return null;
  const a = Date.parse(run.started_at);
  const b = Date.parse(run.ended_at);
  if (!Number.isFinite(a) || !Number.isFinite(b) || b < a) return null;
  return Math.round((b - a) / 1000);
}
