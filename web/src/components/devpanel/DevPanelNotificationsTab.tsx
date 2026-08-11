import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { BellRing, ExternalLink, TriangleAlert } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type NotifyTypeStat, type UnifiedAuditEvent } from '@/lib/api';
import { Empty, Skeleton } from '@/components/mds';
import { useDevPanelIdJump } from './id-jump';

const FAILURES_LIMIT = 20;
const STATS_DAYS = 30;

/** Raw shape of one `channel_failures.jsonl` row, as embedded verbatim in
 *  `UnifiedAuditEvent.details.channel_failure` (see `channel_reply.rs` /
 *  `channel_alerts.rs` — both writers of this file). Every field is optional
 *  because the two writers don't share a struct; a recovery row in
 *  particular carries neither `console_url` nor `session_id`. */
interface ChannelFailureRow {
  readonly agent?: string;
  readonly session_id?: string;
  readonly channel?: string;
  readonly reason?: string;
  readonly error?: string;
  readonly console_url?: string | null;
  readonly doc_url?: string | null;
}

/**
 * 通知 tab — detail view for the notification subsystem: `notify.stats`
 * (W2-8's per-type action-rate table, same RPC as the Reports page card but
 * rendered as a compact table here) plus a recent `channel_failures.jsonl`
 * list surfaced through `audit.unified_log` with each row's `console_url` /
 * `doc_url` rendered as real links (Stripe error-object pattern B2).
 */
export function DevPanelNotificationsTab() {
  const connectionState = useConnectionStore((s) => s.state);

  return (
    <div className="flex flex-col gap-4">
      <NotifyStatsSection connectionState={connectionState} />
      <ChannelFailuresSection connectionState={connectionState} />
    </div>
  );
}

function NotifyStatsSection({ connectionState }: { readonly connectionState: string }) {
  const intl = useIntl();
  const [types, setTypes] = useState<NotifyTypeStat[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;
    setLoading(true);
    setFailed(false);
    api.notify
      .stats(STATS_DAYS)
      .then((res) => {
        if (cancelled) return;
        setTypes(res?.types ?? []);
      })
      .catch(() => {
        if (cancelled) return;
        setFailed(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [connectionState]);

  return (
    <section>
      <h3 className="mb-1.5 text-xs font-semibold text-foreground">
        {intl.formatMessage({ id: 'devpanel.notify.statsTitle' })}
      </h3>
      {loading ? (
        <Skeleton className="h-24 w-full rounded-lg" />
      ) : failed ? (
        <p className="py-3 text-center text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'devpanel.notify.loadError' })}
        </p>
      ) : types.length === 0 ? (
        <p className="py-3 text-center text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'devpanel.notify.statsEmpty' }, { days: STATS_DAYS })}
        </p>
      ) : (
        <div className="overflow-hidden rounded-lg border border-surface-border">
          <table className="w-full text-xs">
            <thead className="bg-muted/50 text-muted-foreground">
              <tr>
                <th className="px-2 py-1.5 text-left font-medium">
                  {intl.formatMessage({ id: 'devpanel.notify.col.type' })}
                </th>
                <th className="px-2 py-1.5 text-right font-medium">
                  {intl.formatMessage({ id: 'reports.notify.col.pushed' })}
                </th>
                <th className="px-2 py-1.5 text-right font-medium">
                  {intl.formatMessage({ id: 'reports.notify.col.acted' })}
                </th>
                <th className="px-2 py-1.5 text-right font-medium">
                  {intl.formatMessage({ id: 'devpanel.notify.col.actionRate' })}
                </th>
              </tr>
            </thead>
            <tbody>
              {types.map((t) => (
                <tr key={t.type} className="border-t border-surface-border">
                  <td className="max-w-40 truncate px-2 py-1.5 font-mono" title={t.type}>
                    <span className="inline-flex items-center gap-1">
                      {t.broken && (
                        <TriangleAlert
                          className="size-3 shrink-0 text-destructive"
                          aria-label={intl.formatMessage({ id: 'devpanel.notify.col.broken' })}
                        />
                      )}
                      {t.type}
                    </span>
                  </td>
                  <td className="px-2 py-1.5 text-right font-mono tabular-nums">{t.pushed}</td>
                  <td className="px-2 py-1.5 text-right font-mono tabular-nums text-muted-foreground">
                    {t.actionable > 0
                      ? `${t.acted}/${t.actionable}`
                      : intl.formatMessage({ id: 'reports.notify.fyiOnly' })}
                  </td>
                  <td
                    className={cn(
                      'px-2 py-1.5 text-right font-mono tabular-nums',
                      t.broken && 'font-semibold text-destructive',
                    )}
                  >
                    {t.actionable > 0 ? `${Math.round(t.action_rate * 100)}%` : '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function ChannelFailuresSection({ connectionState }: { readonly connectionState: string }) {
  const intl = useIntl();
  const { jumpToAgent, jumpToConversation } = useDevPanelIdJump();
  const [events, setEvents] = useState<UnifiedAuditEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;
    setLoading(true);
    setFailed(false);
    api.audit
      .unifiedLog({ sources: ['channel_failure'], limit: FAILURES_LIMIT })
      .then((res) => {
        if (cancelled) return;
        setEvents(res?.events ?? []);
      })
      .catch(() => {
        if (cancelled) return;
        setFailed(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [connectionState]);

  return (
    <section>
      <h3 className="mb-1.5 text-xs font-semibold text-foreground">
        {intl.formatMessage({ id: 'devpanel.notify.failuresTitle' })}
      </h3>
      {loading ? (
        <Skeleton className="h-24 w-full rounded-lg" />
      ) : failed ? (
        <p className="py-3 text-center text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'devpanel.notify.loadError' })}
        </p>
      ) : events.length === 0 ? (
        <Empty
          icon={BellRing}
          title={intl.formatMessage({ id: 'devpanel.notify.failuresEmpty' })}
          className="py-6"
        />
      ) : (
        <div className="space-y-1">
          {events.map((evt, i) => {
            const row = (evt.details?.channel_failure ?? {}) as ChannelFailureRow;
            return (
              <div
                key={`${evt.timestamp}-${i}`}
                className="rounded-lg border border-surface-border bg-surface px-2.5 py-1.5 text-xs"
              >
                <div className="flex flex-wrap items-center gap-1.5">
                  {row.channel && (
                    <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground">
                      {row.channel}
                    </span>
                  )}
                  {row.agent && (
                    <button
                      type="button"
                      onClick={() => jumpToAgent(row.agent!)}
                      className="font-mono text-[11px] font-medium text-brand hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                    >
                      {row.agent}
                    </button>
                  )}
                  {row.session_id && (
                    <button
                      type="button"
                      onClick={() => void jumpToConversation(row.session_id!)}
                      className="truncate font-mono text-[10px] text-muted-foreground hover:text-brand hover:underline"
                      title={row.session_id}
                    >
                      {row.session_id}
                    </button>
                  )}
                  <span className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground">
                    {(() => {
                      try {
                        return new Date(evt.timestamp).toLocaleTimeString('zh-TW', {
                          hour12: false,
                          hour: '2-digit',
                          minute: '2-digit',
                          second: '2-digit',
                        });
                      } catch {
                        return evt.timestamp;
                      }
                    })()}
                  </span>
                </div>
                {evt.summary && <p className="mt-1 text-foreground">{evt.summary}</p>}
                {(row.console_url || row.doc_url) && (
                  <div className="mt-1 flex items-center gap-3">
                    {row.console_url && (
                      <a
                        href={row.console_url}
                        target="_blank"
                        rel="noreferrer"
                        className="inline-flex items-center gap-1 text-[11px] font-medium text-brand hover:underline"
                      >
                        <ExternalLink className="size-3" aria-hidden="true" />
                        {intl.formatMessage({ id: 'devpanel.notify.consoleLink' })}
                      </a>
                    )}
                    {row.doc_url && (
                      <a
                        href={row.doc_url}
                        target="_blank"
                        rel="noreferrer"
                        className="inline-flex items-center gap-1 text-[11px] font-medium text-muted-foreground hover:text-brand hover:underline"
                      >
                        <ExternalLink className="size-3" aria-hidden="true" />
                        {intl.formatMessage({ id: 'devpanel.notify.docLink' })}
                      </a>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}
