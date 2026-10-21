import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { History, Loader2 } from 'lucide-react';
import { Badge, Button, Empty, ErrorState } from '@/components/mds';
import { cn } from '@/lib/utils';
import { knowledgeApi, type KbAccessRow } from '@/lib/knowledge-api';
import { principalLabels } from '@/components/knowledge/PrincipalPicker';
import { formatWhen } from './labels';

type Intl = ReturnType<typeof useIntl>;

interface RequesterLike {
  requester_kind: string;
  requester_id: string | null;
  agent_id: string | null;
  channel: string | null;
}

/** "Who asked" in plain words: the person, the AI employee, or the channel. */
export function requesterText(intl: Intl, r: RequesterLike, labels?: Map<string, string>): string {
  const t = (id: string, v?: Record<string, string>) => intl.formatMessage({ id }, v);
  const person = (() => {
    switch (r.requester_kind) {
      case 'dashboard_user':
      case 'channel_user': {
        const id = r.requester_id ?? '';
        return labels?.get(`user:${id}`) ?? (id ? t('kb.log.who.user', { id: id.slice(0, 8) }) : t('kb.log.who.someone'));
      }
      case 'channel_unbound':
        return t('kb.log.who.unbound', { channel: r.channel ? t(`kb.channel.${r.channel}`) : '' });
      case 'unknown':
        return t('kb.log.who.unknown');
      default:
        return t('kb.log.who.system');
    }
  })();
  if (!r.agent_id) return person;
  const agent = labels?.get(`agent:${r.agent_id}`) ?? r.agent_id;
  return t('kb.log.who.agentFor', { agent, person });
}

const OUTCOME_TONE: Record<KbAccessRow['outcome'], string> = {
  ok: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-400',
  no_match: 'bg-muted text-muted-foreground',
  denied: 'bg-destructive/10 text-destructive',
  error: 'bg-amber-500/15 text-amber-700 dark:text-amber-400',
};

/**
 * 存取紀錄 (owners): who looked at this collection, through which AI
 * employee or channel, what they asked and how many passages came back.
 */
export function AccessLogTab({ collectionId }: { collectionId: string }) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [rows, setRows] = useState<KbAccessRow[] | null>(null);
  const [labels, setLabels] = useState<Map<string, string>>(new Map());
  const [error, setError] = useState<unknown>(null);
  const [more, setMore] = useState(false);
  const [loading, setLoading] = useState(false);

  const load = useCallback(
    async (before?: number) => {
      setLoading(true);
      try {
        setError(null);
        const r = await knowledgeApi.accessLog(collectionId, before);
        setRows((prev) => (before === undefined ? r.entries : [...(prev ?? []), ...r.entries]));
        setMore(r.entries.length >= 50);
      } catch (e) {
        setError(e);
      } finally {
        setLoading(false);
      }
    },
    [collectionId],
  );

  useEffect(() => {
    void load();
  }, [load]);

  // Names for the people in these rows (only people on the collection's
  // list resolve for an owner; others keep a short id).
  const userIds = Array.from(
    new Set((rows ?? []).filter((r) => r.requester_id && r.requester_kind !== 'channel_unbound').map((r) => r.requester_id as string)),
  ).join('\u0000');
  useEffect(() => {
    const ids = userIds ? userIds.split('\u0000') : [];
    knowledgeApi
      .searchPrincipals(collectionId, '', ids)
      .then((p) => setLabels(principalLabels(p)))
      .catch(() => {
        /* Names are a nicety; ids still show. */
      });
  }, [collectionId, userIds]);

  if (error != null && rows === null) return <ErrorState error={error} onRetry={() => void load()} />;
  if (rows === null) {
    return (
      <div className="flex justify-center py-10">
        <Loader2 aria-hidden className="size-5 text-muted-foreground motion-safe:animate-spin" />
        <span className="sr-only">{t('common.loading')}</span>
      </div>
    );
  }
  if (rows.length === 0) {
    return <Empty icon={History} title={t('kb.log.empty')} description={t('kb.log.emptyDesc')} />;
  }
  return (
    <div className="space-y-3">
      <div className="overflow-x-auto rounded-xl border border-surface-border">
        <table className="w-full min-w-[640px] text-sm">
          <caption className="sr-only">{t('kb.tab.log')}</caption>
          <thead className="bg-muted/40 text-left text-xs text-muted-foreground">
            <tr>
              <th scope="col" className="px-3 py-2 font-medium">{t('kb.log.col.when')}</th>
              <th scope="col" className="px-3 py-2 font-medium">{t('kb.log.col.who')}</th>
              <th scope="col" className="px-3 py-2 font-medium">{t('kb.log.col.what')}</th>
              <th scope="col" className="px-3 py-2 font-medium">{t('kb.log.col.query')}</th>
              <th scope="col" className="px-3 py-2 text-right font-medium">{t('kb.log.col.results')}</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-surface-border">
            {rows.map((r) => (
              <tr key={r.id}>
                <td className="whitespace-nowrap px-3 py-2 font-mono text-xs tabular-nums text-muted-foreground">
                  {formatWhen(intl, r.at)}
                </td>
                <td className="max-w-48 truncate px-3 py-2" title={requesterText(intl, r, labels)}>
                  {requesterText(intl, r, labels)}
                </td>
                <td className="whitespace-nowrap px-3 py-2">
                  <span className="mr-1.5">{t(`kb.op.${r.op}`)}</span>
                  {r.outcome !== 'ok' && (
                    <Badge variant="ghost" className={cn(OUTCOME_TONE[r.outcome])}>
                      {t(`kb.outcome.${r.outcome}`)}
                    </Badge>
                  )}
                </td>
                <td
                  className="max-w-64 truncate px-3 py-2 text-muted-foreground"
                  title={r.query ?? (r.query_hash ? t('kb.log.hashOnly') : '')}
                >
                  {r.query ?? (r.query_hash ? <span className="font-mono text-xs">#{r.query_hash}</span> : '—')}
                </td>
                <td className="px-3 py-2 text-right font-mono text-xs tabular-nums">{r.result_count}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {more && (
        <div className="flex justify-center">
          <Button variant="outline" size="sm" disabled={loading} onClick={() => void load(rows[rows.length - 1]?.id)}>
            {t('kb.log.more')}
          </Button>
        </div>
      )}
    </div>
  );
}
