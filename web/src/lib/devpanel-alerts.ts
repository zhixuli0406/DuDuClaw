import type { UnifiedAuditEvent } from './api';

/**
 * Developer panel (W3-4) critical-alert classification — Stripe Workbench
 * pattern E2: "the taskbar includes a notification tray that alerts you to
 * critical API errors" even after the pane is collapsed down to a taskbar or
 * an icon (03-analogous-products.md §7). The panel's badge reuses the
 * already-fetched `audit.unified_log` RPC — no new backend surface.
 *
 * `audit.unified_log` only ever labels `channel_failure` rows `warning`
 * (never `critical` — see `handle_audit_unified_log` in
 * `crates/duduclaw-gateway/src/handlers.rs`), so a plain
 * `severity === 'critical'` check would silently never fire for the one
 * source operators care about mid-outage. Two escalation rules close that
 * gap without touching the backend:
 *
 *  1. Any event the backend already labels `critical` always counts (today
 *     only `security` rows can carry that severity).
 *  2. A `channel_failure` row whose underlying `FailureReason` (see
 *     `channel_reply.rs`) is a billing/budget-exhaustion class — `Billing`
 *     (402 / insufficient_quota) or `AccountsCoolingDownLong` (every account
 *     stuck in the 24h billing-class cooldown) — escalates to critical for
 *     badge purposes. A stalled subscription is exactly the kind of thing
 *     that must reach someone even if they never open the events tab
 *     (pattern F8: "能力≠觸達" — capability without reach is nothing).
 *
 * Deliberately an exact-match allowlist against the known Rust enum tokens,
 * not a substring/regex guess — the coding convention in this repo is exact
 * token equality over unanchored `contains` for anything routing/classifying
 * on a machine-generated string.
 *
 * Pure and side-effect free so the badge logic is unit-testable without
 * mounting the panel.
 */
const BUDGET_CLASS_REASONS: ReadonlySet<string> = new Set([
  'Billing',
  'AccountsCoolingDownLong',
]);

export function isDevPanelCriticalEvent(event: UnifiedAuditEvent): boolean {
  if (event.severity === 'critical') return true;
  if (event.source !== 'channel_failure') return false;
  const row = event.details?.channel_failure as { reason?: unknown } | undefined;
  const reason = typeof row?.reason === 'string' ? row.reason : '';
  return BUDGET_CLASS_REASONS.has(reason);
}

/**
 * Latest timestamp among the critical-classified events in `events`, or
 * `null` when none qualify. Does not assume any particular input order —
 * takes a max over RFC3339 strings, which sort correctly as plain strings
 * (fixed-width, UTC `Z` suffix, per `audit.unified_log`'s own sort).
 */
export function latestCriticalTimestamp(
  events: readonly UnifiedAuditEvent[],
): string | null {
  let latest: string | null = null;
  for (const event of events) {
    if (!event.timestamp) continue;
    if (!isDevPanelCriticalEvent(event)) continue;
    if (latest === null || event.timestamp > latest) latest = event.timestamp;
  }
  return latest;
}
