/**
 * Home overview aggregation (W2-1, `05-ux-redesign-proposal.md` Wave 2 /
 * `02-ux-methodology.md` §P3-1). Pure logic behind the Home page's fixed
 * "overview first" section — one health-summary line ("N 位 AI 員工待命，M 件
 * 等你決定，K 件沒送出去") plus the "需要你處理" list beneath it.
 *
 * This is deliberately separate from the customizable widget system
 * (`HomePage.tsx` / `NeedsMeRow`) — it is a fixed, always-visible read of the
 * same class of signal, not another draggable widget, and it exists so the
 * first screen can answer "is anything wrong, and which thing" without
 * scrolling (憲章 8, Shneiderman "overview first").
 *
 * Data sources are deliberately narrow, per spec: `agents.list` for N,
 * `approvals.list` + `tasks.list({status:'needs_human'})` for M, and
 * `audit.unified_log` (channel_failure source) for K. No backend change; the
 * component that calls these functions owns the actual RPCs.
 */
import type { IntlShape } from 'react-intl';
import type { RiskLevel } from './approval-risk';
import { TYPE_URGENCY, sortInbox, type InboxItem } from './inbox-model';

/**
 * Most `channel_failures.jsonl` producers (`channel_reply.rs` —
 * `channel_reply_fallback` / `channel_reply_silent` / `pty_pool_fallback`)
 * carry no resolved/acked flag and no `channel` field at all, so recency is
 * still the least-wrong proxy for those rows: a send failure with nothing
 * since is presumed either fixed or superseded. 24h is deliberately generous
 * — long enough to survive a weekend gap, short enough that a months-old
 * one-off failure never resurfaces as "K 件沒送出去" on an otherwise healthy
 * install.
 *
 * W2-8: `channel_alerts.rs`'s own failures DO carry a `channel`, and that
 * module now also writes an explicit `channel_recovered` resolution row —
 * see {@link excludeRecoveredChannelFailures}, which precise-ifies this proxy
 * for exactly that subset before it ever reaches the 24h check below.
 */
export const UNDELIVERED_WINDOW_MS = 24 * 60 * 60 * 1000;

/**
 * N — AI staff counted as "standing by": active, not archived. Mirrors the
 * roster default (`agents.list` already excludes archived unless
 * `include_archived` is passed) — `archived` only matters for a caller that
 * fetched the full roster explicitly. `status` is a plain `string` (not
 * `AgentInfo['status']`) so this stays decoupled from the roster's exact
 * union — the only thing that matters here is the literal `'active'` value.
 */
export function countStandbyAgents(agents: ReadonlyArray<{ status: string; archived?: boolean }>): number {
  return agents.filter((a) => a.status === 'active' && !a.archived).length;
}

/**
 * Keep only items whose ISO `timestamp` falls within `windowMs` before
 * `nowMs` (and not in the future — an unparseable or clock-skewed timestamp
 * is dropped rather than trusted). Generic so it can gate any recency-scoped
 * list, not just channel failures.
 */
export function withinRecentWindow<T extends { timestamp: string }>(
  items: ReadonlyArray<T>,
  nowMs: number,
  windowMs: number,
): T[] {
  return items.filter((item) => {
    const t = Date.parse(item.timestamp);
    return Number.isFinite(t) && t <= nowMs && nowMs - t <= windowMs;
  });
}

/**
 * W2-8 — the `channel_failure`-source unified-log row shape. `handlers.rs`'s
 * `handle_audit_unified_log` nests the raw `channel_failures.jsonl` record
 * under `details.channel_failure` (NOT `details.channel` directly — reading
 * the flatter path was a bug three call sites shared, see
 * {@link channelFailureChannel} below).
 */
export interface ChannelFailureAuditEvent {
  timestamp: string;
  event_type: string;
  agent_id: string;
  summary: string;
  // `Record<string, unknown>` (not a nested literal type) so this stays
  // structurally assignable from `UnifiedAuditEvent` — `channel_failure`'s
  // shape is checked at runtime in `channelFailureChannel` below instead.
  details?: Record<string, unknown>;
}

/** Failure payloads contain diagnostic JSON, never a user-facing sentence. */
export function failedRunTitle(event: Pick<ChannelFailureAuditEvent, 'agent_id'>, formatMessage: IntlShape['formatMessage'], agentName = event.agent_id): string {
  return formatMessage({ id: 'inbox.failedRun.title' }, { agent: agentName });
}

/** Share the same run destination across Inbox and Home. */
export function failedRunHref(item: { runId?: string; agentId?: string }): string {
  return item.runId ? `/runs?run=${encodeURIComponent(item.runId)}`
    : item.agentId ? `/runs?agent=${encodeURIComponent(item.agentId)}` : '/runs';
}

export function failureRunId(event: ChannelFailureAuditEvent): string | undefined {
  const inner = event.details?.channel_failure;
  const nested = inner && typeof inner === 'object' ? (inner as Record<string, unknown>).run_id : undefined;
  const id = event.details?.run_id ?? nested;
  return typeof id === 'string' && id ? id : undefined;
}

/** Legacy failure records stored a display name rather than the roster id. */
export function resolveFailureAgentId(value: string, agents: ReadonlyArray<{ name: string; display_name?: string }>): string | undefined {
  if (agents.some((agent) => agent.name === value)) return value;
  const matches = agents.filter((agent) => agent.display_name === value);
  return matches.length === 1 ? matches[0].name : undefined;
}

/** Unified-log summaries share the same plain words as the Inbox. */
export function auditEventSummary(event: ChannelFailureAuditEvent & { source: string }, formatMessage: IntlShape['formatMessage'], agentName = event.agent_id): string {
  return event.source === 'channel_failure' && /^[\s]*[\[{]/.test(event.summary) && !isChannelRecoveredEvent(event.event_type)
    ? failedRunTitle(event, formatMessage, agentName)
    : event.summary;
}

/**
 * Correctly extract the channel a `channel_failure`-source unified-log row is
 * about. `handlers.rs` nests the raw JSONL row under
 * `details.channel_failure` — reading `details.channel` (as HealthOverview,
 * InboxPage, and the Home "needs me" widget each did independently until
 * W2-8) always returns `undefined`, silently no-op'ing the channel-grouping
 * feature those pages otherwise wire it up for.
 */
export function channelFailureChannel(ev: ChannelFailureAuditEvent): string | undefined {
  const inner = ev.details?.channel_failure;
  if (!inner || typeof inner !== 'object') return undefined;
  const c = (inner as Record<string, unknown>).channel;
  return typeof c === 'string' && c !== '' ? c : undefined;
}

/**
 * `channel_alerts.rs`'s `record_recovery` appends an explicit
 * `channel_recovered` row (rendered here as `event_type: "channel.recovered"`
 * — see `handlers.rs`) once a channel drops back under its failure
 * threshold. It is a RESOLUTION, not a failure, and must never itself become
 * a "沒送出去" attention item.
 */
export function isChannelRecoveredEvent(eventType: string): boolean {
  return eventType === 'channel.recovered';
}

/**
 * W2-8 — precise-ify the 24h recency proxy below with the explicit
 * `channel_recovered` signal `channel_alerts.rs` now emits: drop (1) the
 * recovery rows themselves (never an attention item — see
 * {@link isChannelRecoveredEvent}) and (2) any failure row for a channel that
 * has since recovered (a later `channel_recovered` row for the SAME
 * channel), regardless of whether it's still inside the 24h window. Rows
 * with no resolvable channel (most current writers still omit one — see the
 * handoff note in `HealthOverview.tsx`) are left untouched for the caller's
 * existing {@link withinRecentWindow} proxy to judge, so this can only ever
 * SHRINK false positives, never manufacture a false negative.
 */
export function excludeRecoveredChannelFailures<T extends ChannelFailureAuditEvent>(
  events: ReadonlyArray<T>,
): T[] {
  const latestRecoveryMsByChannel = new Map<string, number>();
  for (const ev of events) {
    if (!isChannelRecoveredEvent(ev.event_type)) continue;
    const channel = channelFailureChannel(ev);
    if (!channel) continue;
    const t = Date.parse(ev.timestamp);
    if (!Number.isFinite(t)) continue;
    const prev = latestRecoveryMsByChannel.get(channel);
    if (prev === undefined || t > prev) latestRecoveryMsByChannel.set(channel, t);
  }
  return events.filter((ev) => {
    if (isChannelRecoveredEvent(ev.event_type)) return false;
    const channel = channelFailureChannel(ev);
    if (!channel) return true;
    const recoveredAtMs = latestRecoveryMsByChannel.get(channel);
    if (recoveredAtMs === undefined) return true;
    const t = Date.parse(ev.timestamp);
    return !(Number.isFinite(t) && t <= recoveredAtMs);
  });
}

/** A pre-resolved (already localized) source row for {@link buildAttentionItems}. */
export interface AttentionSourceRow {
  id: string;
  title: string;
  agentId?: string;
  timestamp?: string;
  expiresAt?: number | null;
  risk?: RiskLevel;
  /** failed_run rows only — best-effort, see `HealthOverview.tsx` for why
   *  this is usually absent (backend gap, not this module's doing). */
  channel?: string;
  runId?: string;
}

/**
 * Build the "需要你處理" item list — approvals + needs_human tasks (M) plus
 * recent channel-send failures (K) — pre-sorted by urgency. The health-line
 * counts are derived from this SAME list (see {@link overviewCounts}) so the
 * summary number and the row count beneath it can never drift apart.
 */
export function buildAttentionItems(
  approvals: ReadonlyArray<AttentionSourceRow>,
  needsHumanTasks: ReadonlyArray<AttentionSourceRow>,
  undeliveredChannels: ReadonlyArray<AttentionSourceRow>,
): InboxItem[] {
  const items: InboxItem[] = [
    ...approvals.map(
      (a): InboxItem => ({
        id: `approval:${a.id}`,
        type: 'approval',
        title: a.title,
        agentId: a.agentId,
        timestamp: a.timestamp,
        urgency: TYPE_URGENCY.approval,
        actionable: true,
        status: 'pending',
        expiresAt: a.expiresAt,
        risk: a.risk,
      }),
    ),
    ...needsHumanTasks.map(
      (task): InboxItem => ({
        id: `blocked:${task.id}`,
        type: 'blocked',
        title: task.title,
        agentId: task.agentId,
        timestamp: task.timestamp,
        urgency: TYPE_URGENCY.blocked,
        actionable: true,
        status: 'needs_human',
      }),
    ),
    ...undeliveredChannels.map(
      (fr): InboxItem => ({
        id: `failed_run:${fr.id}`,
        type: 'failed_run',
        title: fr.title,
        agentId: fr.agentId,
        channel: fr.channel,
        runId: fr.runId,
        timestamp: fr.timestamp,
        urgency: TYPE_URGENCY.failed_run,
        actionable: false,
        status: 'warning',
      }),
    ),
  ];
  return sortInbox(items, 'urgency');
}

export interface OverviewCounts {
  standby: number;
  /** M — approvals + needs_human tasks ("等你決定"). */
  pendingDecisions: number;
  /** K — recent channel-send failures ("沒送出去"). */
  undelivered: number;
}

/**
 * Derive the health-line counts from the same item list the "需要你處理"
 * section renders (see {@link buildAttentionItems}) — a structural guarantee
 * that the summary number and the list beneath it always agree.
 */
export function overviewCounts(standby: number, items: ReadonlyArray<InboxItem>): OverviewCounts {
  let pendingDecisions = 0;
  let undelivered = 0;
  for (const item of items) {
    if (item.type === 'approval' || item.type === 'blocked') pendingDecisions += 1;
    else if (item.type === 'failed_run') undelivered += 1;
  }
  return { standby, pendingDecisions, undelivered };
}

/** All-clear — the calm state the health line collapses to (green-dominant,
 *  spec: "全綠時顯示安心態"). */
export function isAllClear(counts: OverviewCounts): boolean {
  return counts.pendingDecisions === 0 && counts.undelivered === 0;
}
