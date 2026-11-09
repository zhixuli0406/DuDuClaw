import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { ScrollText } from 'lucide-react';
import { api, type AuditEvent } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import {
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Empty,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/mds';
import { cn } from '@/lib/utils';
import { Refusal, TableBox, formatWhen, refusalMessage, useOptionalT, usePersonName, useProviderName, useT } from './shared';

/** How many recent security records to scan for protected-mode events. */
const SCAN_LIMIT = 1000;

export const AUDIT_CATEGORIES = ['all', 'connector', 'employee', 'restore', 'delegation'] as const;
export type AuditCategory = (typeof AUDIT_CATEGORIES)[number];

/**
 * Which 紀錄 filter an event belongs to; `null` = not a protected-mode event.
 * Restore/purge and delegation events are matched by name so records written
 * by later backend tasks (F6 restore, delegation decisions) appear without a
 * dashboard change.
 */
export function auditCategory(eventType: string): Exclude<AuditCategory, 'all'> | null {
  if (!eventType.startsWith('isolation.')) return null;
  const rest = eventType.slice('isolation.'.length);
  if (/restor|purg|quarantine/.test(rest)) return 'restore';
  if (rest.includes('delegation')) return 'delegation';
  if (rest.startsWith('connector_')) return 'connector';
  if (rest.startsWith('employee_')) return 'employee';
  return null;
}

export function AuditTab() {
  const t = useT();
  const optional = useOptionalT();
  const intl = useIntl();
  const connection = useConnectionStore((s) => s.state);
  const personName = usePersonName();
  const providerName = useProviderName();
  const [events, setEvents] = useState<AuditEvent[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [category, setCategory] = useState<AuditCategory>('all');
  const generation = useRef(0);

  const load = useCallback(async () => {
    const current = ++generation.current;
    try {
      const result = await api.security.auditLog(SCAN_LIMIT);
      if (current !== generation.current) return;
      setEvents(result?.events ?? []);
      setLoadError(null);
    } catch (e) {
      if (current !== generation.current) return;
      setEvents(null);
      setLoadError(refusalMessage(e));
    }
  }, []);

  useEffect(() => {
    if (connection === 'authenticated') void load();
    return () => { generation.current += 1; };
  }, [connection, load]);

  const rows = useMemo(() => {
    const relevant = (events ?? [])
      .map((event) => ({ event, category: auditCategory(event.event_type) }))
      .filter((row): row is { event: AuditEvent; category: Exclude<AuditCategory, 'all'> } => row.category !== null);
    relevant.sort((a, b) => b.event.timestamp.localeCompare(a.event.timestamp));
    return relevant;
  }, [events]);
  const visible = category === 'all' ? rows : rows.filter((row) => row.category === category);

  const what = (event: AuditEvent, kind: Exclude<AuditCategory, 'all'>): string => {
    const name = event.event_type.slice('isolation.'.length);
    const title = optional(`audit.event.${name}`, t(`audit.generic.${kind}`));
    const details = event.details ?? {};
    // Connector rows name the service; employee rows name the employee
    // (the record's subject). Service-internal source names never show.
    const subject =
      kind === 'connector'
        ? typeof details.provider === 'string' ? providerName(details.provider) : null
        : typeof details.label === 'string'
          ? details.label
          : kind !== 'delegation' && event.agent_id && !event.agent_id.startsWith('isolation-') ? event.agent_id : null;
    return subject ? `${title} · ${subject}` : title;
  };

  const who = (event: AuditEvent): string => {
    const actor = event.details?.actor_user_id;
    return typeof actor === 'string' && actor.length > 0 ? personName(actor) : t('audit.system');
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle><h2>{t('audit.title')}</h2></CardTitle>
        <CardDescription>{t('audit.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div role="group" aria-label={t('audit.filter')} className="flex flex-wrap gap-2">
          {AUDIT_CATEGORIES.map((value) => (
            <Button
              key={value}
              size="sm"
              variant="outline"
              aria-pressed={category === value}
              className={cn(category === value && 'border-brand bg-brand/10 text-foreground')}
              onClick={() => setCategory(value)}
            >
              {t(`audit.category.${value}`)}
            </Button>
          ))}
          <Button size="sm" variant="ghost" onClick={() => void load()}>{t('refresh')}</Button>
        </div>

        <Refusal message={loadError} />
        {events === null && !loadError && <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>}
        {events !== null && visible.length === 0 && (
          <Empty variant="dashed" icon={ScrollText} title={t('audit.empty')} description={t('audit.emptyHint')} />
        )}
        {visible.length > 0 && (
          <TableBox label={t('audit.title')}>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead scope="col">{t('audit.col.when')}</TableHead>
                  <TableHead scope="col">{t('audit.col.what')}</TableHead>
                  <TableHead scope="col">{t('audit.col.who')}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {visible.map(({ event, category: kind }, index) => (
                  <TableRow key={`${event.timestamp}:${event.event_type}:${index}`}>
                    <TableCell>{formatWhen(event.timestamp, intl.locale)}</TableCell>
                    <TableCell className="whitespace-normal">{what(event, kind)}</TableCell>
                    <TableCell>{who(event)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </TableBox>
        )}
      </CardContent>
    </Card>
  );
}
