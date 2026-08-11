import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type TickRecordEntry, type TickSourceStatus, type TicksSourcesResult } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Badge, Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Empty, Spinner } from '@/components/mds';
import { ChevronDown, ChevronRight, Radio, RefreshCw } from 'lucide-react';
import { cn } from '@/lib/utils';

// Resident sensing observability (WP4) — read-only card on the automation
// rules page showing the live status of `[[tick.sources]]` config-driven
// data feeds (行情/檔案/指令) and the WP3 local screening layer that gates
// them before waking an agent. Intentionally read-only here: creating /
// enabling a source is a config.toml change, not a dashboard action.

const REFRESH_MS = 15_000;

const KIND_LABEL_KEYS: Record<TickSourceStatus['kind'], string> = {
  http_poll: 'autopilot.monitor.kind.http_poll',
  command: 'autopilot.monitor.kind.command',
  file_tail: 'autopilot.monitor.kind.file_tail',
  websocket: 'autopilot.monitor.kind.websocket',
};

function droppedTotal(d: TickSourceStatus['dropped']): number {
  return d.rate_cap + d.unchanged + d.oversize + d.fetch_error + d.non_text;
}

/** Compact `key=value` rendering of one buffered observation. */
function renderFields(entry: TickRecordEntry): string {
  if (entry.raw !== null && entry.raw !== undefined) return entry.raw;
  const parts = Object.entries(entry.fields).map(([k, v]) => `${k}=${String(v)}`);
  return parts.length > 0 ? parts.join(' ') : '—';
}

function SourceRow({ source }: { source: TickSourceStatus }) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const [records, setRecords] = useState<TickRecordEntry[] | null>(null);
  const [loading, setLoading] = useState(false);

  const toggle = useCallback(async () => {
    const next = !open;
    setOpen(next);
    if (next && records === null) {
      setLoading(true);
      try {
        const result = await api.ticks.recent(source.id);
        setRecords(result.records);
      } catch (e) {
        setRecords([]);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      } finally {
        setLoading(false);
      }
    }
  }, [open, records, source.id, intl]);

  const dropped = droppedTotal(source.dropped);

  return (
    <div className="rounded-lg border border-surface-border">
      <button
        type="button"
        onClick={() => void toggle()}
        className="flex w-full items-center justify-between gap-3 px-4 py-3 text-left transition-colors hover:bg-surface-hover/50"
        aria-expanded={open}
      >
        <div className="flex items-start gap-2">
          {open ? (
            <ChevronDown className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          ) : (
            <ChevronRight className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          )}
          <div>
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-medium text-foreground">{source.id}</span>
              <Badge variant={source.enabled ? 'secondary' : 'ghost'}>
                {intl.formatMessage({
                  id: source.enabled ? 'autopilot.monitor.status.enabled' : 'autopilot.monitor.status.disabled',
                })}
              </Badge>
              <Badge variant="outline">
                {/* A kind this build has no label for (older dashboard, newer
                    gateway) falls back to the raw id instead of throwing. */}
                {KIND_LABEL_KEYS[source.kind]
                  ? intl.formatMessage({ id: KIND_LABEL_KEYS[source.kind] })
                  : source.kind}
              </Badge>
            </div>
            <p className="mt-1 text-xs text-muted-foreground">
              {source.last_tick_ts
                ? intl.formatMessage(
                    { id: 'autopilot.monitor.lastTick' },
                    { time: new Date(source.last_tick_ts).toLocaleString('zh-TW') },
                  )
                : intl.formatMessage({ id: 'autopilot.monitor.lastTick.never' })}
            </p>
          </div>
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1 text-xs text-muted-foreground">
          <span>
            {intl.formatMessage(
              { id: 'autopilot.monitor.eventsPerMinute' },
              { count: Math.round(source.events_per_minute_approx * 10) / 10 },
            )}
          </span>
          <span>{intl.formatMessage({ id: 'autopilot.monitor.totalEvents' }, { count: source.events_emitted_total })}</span>
          {dropped > 0 && (
            <span className="text-warning">
              {intl.formatMessage({ id: 'autopilot.monitor.dropped' }, { count: dropped })}
            </span>
          )}
        </div>
      </button>
      {open && (
        <div className="border-t border-surface-border px-4 py-3">
          {loading ? (
            <div className="flex justify-center py-4">
              <Spinner />
            </div>
          ) : !records || records.length === 0 ? (
            <p className="py-2 text-center text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'autopilot.monitor.recent.empty' })}
            </p>
          ) : (
            <>
              <p className="mb-2 text-xs font-medium text-foreground">
                {intl.formatMessage({ id: 'autopilot.monitor.recent.title' })}
              </p>
              <ul className="max-h-56 space-y-1 overflow-y-auto font-mono text-xs text-muted-foreground">
                {[...records].reverse().map((entry, i) => (
                  <li key={`${entry.ts}-${i}`} className="truncate" title={renderFields(entry)}>
                    <span className="text-foreground">{new Date(entry.ts).toLocaleTimeString('zh-TW')}</span>{' '}
                    {renderFields(entry)}
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export function TickSourcesCard() {
  const intl = useIntl();
  const [data, setData] = useState<TicksSourcesResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);

  const fetchSources = useCallback(async (opts?: { silent?: boolean }) => {
    if (!opts?.silent) setRefreshing(true);
    try {
      const result = await api.ticks.sources();
      setData(result);
    } catch (e) {
      if (!opts?.silent) {
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      }
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, [intl]);

  useEffect(() => {
    fetchSources();
    const t = setInterval(() => void fetchSources({ silent: true }), REFRESH_MS);
    return () => clearInterval(t);
  }, [fetchSources]);

  const sources = data?.sources ?? [];
  const screen = data?.screen;

  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between gap-2">
          <div>
            <CardTitle className="flex items-center gap-2">
              <Radio className="size-4 text-primary" />
              {intl.formatMessage({ id: 'autopilot.monitor.title' })}
            </CardTitle>
            <CardDescription>{intl.formatMessage({ id: 'autopilot.monitor.subtitle' })}</CardDescription>
          </div>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => void fetchSources()}
            title={intl.formatMessage({ id: 'common.refresh' })}
            aria-label={intl.formatMessage({ id: 'common.refresh' })}
          >
            <RefreshCw className={cn('size-4', refreshing && 'animate-spin')} />
          </Button>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {loading ? (
          <div className="flex justify-center py-8">
            <Spinner />
          </div>
        ) : sources.length === 0 ? (
          <Empty
            icon={Radio}
            variant="dashed"
            title={intl.formatMessage({ id: 'autopilot.monitor.empty' })}
            description={intl.formatMessage({ id: 'autopilot.monitor.empty.description' })}
          />
        ) : (
          <>
            {screen && screen.pass + screen.drop + screen.unavailable > 0 && (
              <div className="flex flex-wrap items-center gap-2 rounded-lg border border-surface-border bg-surface-hover/50 px-3 py-2 text-xs">
                <span className="font-medium text-foreground">
                  {intl.formatMessage({ id: 'autopilot.monitor.screen.title' })}
                </span>
                <Badge variant="secondary">
                  {intl.formatMessage({ id: 'autopilot.monitor.screen.pass' }, { count: screen.pass })}
                </Badge>
                <Badge variant="outline">
                  {intl.formatMessage({ id: 'autopilot.monitor.screen.drop' }, { count: screen.drop })}
                </Badge>
                <Badge variant="ghost">
                  {intl.formatMessage({ id: 'autopilot.monitor.screen.unavailable' }, { count: screen.unavailable })}
                </Badge>
              </div>
            )}
            <div className="space-y-2">
              {sources.map((s) => (
                <SourceRow key={s.id} source={s} />
              ))}
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}
