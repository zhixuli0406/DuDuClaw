import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import {
  ActorAvatar,
  Badge,
  CrossLink,
  Empty,
  ErrorState,
  Skeleton,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/mds';
import { BellOff } from 'lucide-react';

// ── Proactive Settings Tab ─────────────────────────────────────

/**
 * WP-D (§2-3) — R-SINGLE-WRITER. This tab used to be a second, fully
 * independent edit form for `[proactive] notify_channel` / `notify_chat_id` /
 * `notify_thread_id` / `quiet_hours` — the exact same four fields `EditAgentPage`
 * "自動化" tab writes, with its own duplicated `isValidQuietHoursFormat`
 * validator (see the removed W2-9 comment in git history). Two write points
 * on one piece of data meant "I changed it here, does it show there too?"
 * confusion with zero technical reason for it to exist. This is now a
 * read-only cross-agent overview; `EditAgentPage`'s automation tab is the
 * sole write point (WP-C's §2-3 note: it already fully owns saving these
 * four fields — nothing here duplicates that logic anymore).
 */

interface AgentProactiveRow {
  name: string;
  displayName: string;
  enabled: boolean;
  notifyChannel: string;
  quietHours: string;
  /**
   * P05: `agents.inspect` failing for one agent used to fall back to
   * `enabled: false` — a value indistinguishable from "proactive notifications
   * are switched off for this teammate". A failed row now says so and is
   * never drawn as a settings state.
   */
  failed: boolean;
}

const FETCH_CONCURRENCY = 4;

function chunked<T>(arr: readonly T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < arr.length; i += size) out.push(arr.slice(i, i + size));
  return out;
}

export function ProactiveTab() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const navigate = useNavigate();
  const { agents, fetchAgents } = useAgentsStore();
  const [rows, setRows] = useState<AgentProactiveRow[]>([]);
  const [loading, setLoading] = useState(true);
  // Bumped by the retry button to re-run the per-agent fan-out.
  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);

  // `agents.list` (the store's own fetch) does not carry the `[proactive]`
  // block — only `agents.inspect` does (see `AgentDetail.proactive`). There is
  // no bulk read, so this fans out one inspect per agent, capped at
  // `FETCH_CONCURRENCY` concurrent calls in waves (same shape as InboxPage's
  // per-agent decision poll) rather than one N-wide burst.
  useEffect(() => {
    if (agents.length === 0) {
      setRows([]);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    (async () => {
      const collected: AgentProactiveRow[] = [];
      for (const wave of chunked(agents, FETCH_CONCURRENCY)) {
        if (cancelled) return;
        const results = await Promise.all(
          wave.map((a) =>
            api.agents
              .inspect(a.name)
              .then((detail) => ({
                name: a.name,
                displayName: a.display_name || a.name,
                enabled: detail?.proactive?.enabled ?? false,
                notifyChannel: detail?.proactive?.notify_channel ?? '',
                quietHours: detail?.proactive?.quiet_hours_own ?? '',
                failed: false,
              }))
              .catch((e) => {
                console.warn('[api]', e);
                return {
                  name: a.name,
                  displayName: a.display_name || a.name,
                  enabled: false,
                  notifyChannel: '',
                  quietHours: '',
                  failed: true,
                };
              }),
          ),
        );
        collected.push(...results);
        if (!cancelled) setRows([...collected]);
      }
      if (!cancelled) setLoading(false);
    })();
    return () => {
      cancelled = true;
    };
    // Re-fan-out only when the roster itself changes (or the user retries),
    // not on every re-render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agents.length, reloadKey]);

  const goEdit = (name: string) => navigate(`/agents/${encodeURIComponent(name)}/edit?tab=automation`);

  const failedCount = rows.filter((r) => r.failed).length;

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{t('proactive.overview.desc')}</p>

      {failedCount > 0 && (
        <ErrorState
          variant="inline"
          title={t('errorState.manage.loadFailed')}
          description={intl.formatMessage(
            { id: 'errorState.manage.partialLoad' },
            { count: failedCount },
          )}
          onRetry={() => setReloadKey((k) => k + 1)}
        />
      )}

      {loading ? (
        <div className="space-y-2">
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-9 w-full" />
        </div>
      ) : rows.length === 0 ? (
        <Empty icon={BellOff} title={t('proactive.overview.empty')} />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t('proactive.overview.col.agent')}</TableHead>
                <TableHead>{t('proactive.overview.col.status')}</TableHead>
                <TableHead>{t('proactive.notifyChannel')}</TableHead>
                <TableHead>{t('proactive.quietHours')}</TableHead>
                <TableHead className="text-right">{t('proactive.overview.col.edit')}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((row) => (
                <TableRow key={row.name}>
                  <TableCell>
                    <span className="flex items-center gap-2">
                      <ActorAvatar actorType="agent" name={row.name} size="sm" />
                      <span className="font-medium text-foreground">{row.displayName}</span>
                    </span>
                  </TableCell>
                  <TableCell>
                    {row.failed ? (
                      <Badge
                        variant="secondary"
                        className="bg-destructive/10 text-destructive"
                        title={t('errorState.manage.rowFailedHint')}
                      >
                        {t('errorState.manage.rowFailed')}
                      </Badge>
                    ) : row.enabled ? (
                      <Badge variant="secondary" className="bg-success/15 text-success">
                        {t('common.enabled')}
                      </Badge>
                    ) : (
                      <Badge variant="outline">{t('common.disabled')}</Badge>
                    )}
                  </TableCell>
                  <TableCell className="text-sm text-muted-foreground">
                    {row.failed ? '—' : row.notifyChannel || t('proactive.overview.notSet')}
                  </TableCell>
                  <TableCell className="font-mono text-xs text-muted-foreground">
                    {row.failed ? '—' : row.quietHours || t('proactive.overview.notSet')}
                  </TableCell>
                  <TableCell className="text-right">
                    <CrossLink label={t('proactive.overview.editLink')} onClick={() => goEdit(row.name)} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}
