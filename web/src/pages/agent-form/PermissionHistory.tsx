import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Lock } from 'lucide-react';
import { api, type BuiltinToolEntry, type PermissionHistoryEvent } from '@/lib/api';
import { Button } from '@/components/mds';
import { fetchCatalog } from '@/components/shared/ToolCatalogPicker';

type StateCode = 'allowed' | 'denied' | 'approval';

export type HistoryLine =
  | { kind: 'tool'; entry: string; from: StateCode; to: StateCode }
  | { kind: 'allowlistOn'; count: number }
  | { kind: 'allowlistOff' }
  | { kind: 'other'; key: string };

const PAGE = 20;

const asList = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);

/**
 * Turn one audited `{key, before, after}` into per-tool lines. The audit keeps
 * the raw lists, so the lines are the entries added to / removed from each.
 * Wildcard entries are shown literally (`mcp__duduclaw__*`) — never expanded.
 */
export function describeChange(change: { key: string; before: unknown; after: unknown }): HistoryLine[] {
  const before = asList(change.before);
  const after = asList(change.after);
  const added = after.filter((e) => !before.includes(e));
  const removed = before.filter((e) => !after.includes(e));
  const lines = (list: string[], from: StateCode, to: StateCode): HistoryLine[] =>
    list.map((entry) => ({ kind: 'tool', entry, from, to }));

  switch (change.key) {
    case 'capabilities.denied_tools':
      return [...lines(added, 'allowed', 'denied'), ...lines(removed, 'denied', 'allowed')];
    case 'capabilities.allowed_tools':
      // Entering / leaving allowlist mode flips the meaning of every other
      // tool, so it is not reducible to per-entry lines.
      if (before.length === 0 && after.length > 0) return [{ kind: 'allowlistOn', count: after.length }];
      if (before.length > 0 && after.length === 0) return [{ kind: 'allowlistOff' }];
      return [...lines(added, 'denied', 'allowed'), ...lines(removed, 'allowed', 'denied')];
    case 'capabilities.approval_required_tools':
      return [...lines(added, 'allowed', 'approval'), ...lines(removed, 'approval', 'allowed')];
    default:
      return [{ kind: 'other', key: change.key.replace(/^(capabilities|permissions)\./, '') }];
  }
}

interface PermissionHistoryProps {
  agentId: string;
  isAdmin: boolean;
  /** Bump to reload the first page (e.g. after the table saved). */
  refreshKey?: number;
}

export function PermissionHistory({ agentId, isAdmin, refreshKey = 0 }: PermissionHistoryProps) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [events, setEvents] = useState<PermissionHistoryEvent[] | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [error, setError] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [catalog, setCatalog] = useState<BuiltinToolEntry[]>([]);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    if (!isAdmin) return;
    let alive = true;
    setError(false);
    api.agents
      .permissionHistory(agentId, { limit: PAGE })
      .then((res) => {
        if (!alive) return;
        setEvents(res.events ?? []);
        setCursor(res.next_cursor ?? null);
      })
      .catch(() => alive && setError(true));
    return () => {
      alive = false;
    };
  }, [agentId, isAdmin, refreshKey, reload]);

  // Friendly names are best-effort: without the catalog, raw entries show.
  useEffect(() => {
    let alive = true;
    fetchCatalog()
      .then((l) => alive && setCatalog(l))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  const loadMore = useCallback(() => {
    if (!cursor) return;
    setLoadingMore(true);
    api.agents
      .permissionHistory(agentId, { limit: PAGE, cursor })
      .then((res) => {
        setEvents((prev) => [...(prev ?? []), ...(res.events ?? [])]);
        setCursor(res.next_cursor ?? null);
      })
      .catch(() => setError(true))
      .finally(() => setLoadingMore(false));
  }, [agentId, cursor]);

  const nameOf = (entry: string) => {
    const hit = catalog.find((c) => c.qualified === entry || c.name === entry);
    return hit?.name ?? entry;
  };
  const stateText = (s: StateCode) => t(`agents.perm.state.${s}`);

  if (!isAdmin) {
    return (
      <p className="flex items-start gap-2 rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
        <Lock className="mt-0.5 size-3.5 shrink-0" />
        {t('agents.perm.history.adminOnly')}
      </p>
    );
  }
  if (error && !events) {
    return (
      <div role="alert" className="space-y-2 rounded-lg border border-border px-4 py-6 text-center">
        <p className="text-sm font-medium text-foreground">{t('agents.perm.history.error.title')}</p>
        <p className="text-xs text-muted-foreground">{t('agents.perm.history.error.desc')}</p>
        <Button variant="outline" size="sm" onClick={() => setReload((k) => k + 1)}>
          {t('agents.perm.matrix.retry')}
        </Button>
      </div>
    );
  }
  if (!events) {
    return <p className="px-2 py-6 text-center text-xs text-muted-foreground">{t('agents.perm.history.loading')}</p>;
  }
  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-border px-4 py-8 text-center">
        <p className="text-sm font-medium text-foreground">{t('agents.perm.history.empty.title')}</p>
        <p className="mt-1 text-xs text-muted-foreground">{t('agents.perm.history.empty.desc')}</p>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border">
        {events.map((ev) => (
          <li key={`${ev.timestamp}-${ev.actor}`} className="space-y-1.5 px-3 py-2.5">
            <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
              <time dateTime={ev.timestamp}>{new Date(ev.timestamp).toLocaleString(intl.locale)}</time>
              <span className="font-medium text-foreground">{ev.actor_name ?? ev.actor}</span>
            </div>
            <ul className="space-y-0.5 text-xs">
              {ev.changes.flatMap((c) => describeChange(c)).map((line, i) => (
                <li key={i} className="flex flex-wrap items-center gap-1.5">
                  {line.kind === 'tool' && (
                    <>
                      <span className="font-mono font-medium text-foreground">{nameOf(line.entry)}</span>
                      <s className="text-muted-foreground">{stateText(line.from)}</s>
                      <span aria-hidden>→</span>
                      <span className="text-foreground">{stateText(line.to)}</span>
                    </>
                  )}
                  {line.kind === 'allowlistOn' && (
                    <span className="text-foreground">{t('agents.perm.history.allowlistOn', { n: line.count })}</span>
                  )}
                  {line.kind === 'allowlistOff' && (
                    <span className="text-foreground">{t('agents.perm.history.allowlistOff')}</span>
                  )}
                  {line.kind === 'other' && (
                    <span className="text-foreground">{t('agents.perm.history.other', { key: line.key })}</span>
                  )}
                </li>
              ))}
            </ul>
          </li>
        ))}
      </ul>
      {error && <p className="text-center text-xs text-destructive">{t('agents.perm.history.error.desc')}</p>}
      {cursor && (
        <div className="flex justify-center">
          <Button variant="outline" size="sm" disabled={loadingMore} onClick={loadMore}>
            {t('agents.perm.history.more')}
          </Button>
        </div>
      )}
    </div>
  );
}
