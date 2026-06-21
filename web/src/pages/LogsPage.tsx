import { useEffect, useRef, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useLogsStore, selectFilteredEntries } from '@/stores/logs-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { toast, formatError } from '@/lib/toast';
import {
  api,
  type UnifiedAuditEvent,
  type UnifiedAuditSource,
} from '@/lib/api';
import {
  Pause,
  Play,
  Trash2,
  History,
  Radio,
  FileText,
  Inbox,
} from 'lucide-react';
import {
  Page,
  PageHeader,
  Card,
  Tabs,
  Button,
  Badge,
  EmptyState,
  Toolbar,
  controlClass,
} from '@/components/ui';
import type { TabItem } from '@/components/ui';

// ── Shared styles ──────────────────────────────────────────

const levelStyles: Record<string, string> = {
  trace: 'text-stone-400 dark:text-stone-500',
  debug: 'text-stone-500 dark:text-stone-400',
  info: 'text-blue-600 dark:text-blue-400',
  warn: 'text-amber-600 dark:text-amber-400',
  error: 'text-rose-600 dark:text-rose-400',
};

const levelBg: Record<string, string> = {
  trace: 'bg-stone-100 dark:bg-stone-800',
  debug: 'bg-stone-100 dark:bg-stone-800',
  info: 'bg-blue-50 dark:bg-blue-900/20',
  warn: 'bg-amber-50 dark:bg-amber-900/20',
  error: 'bg-rose-50 dark:bg-rose-900/20',
};

type Tab = 'history' | 'realtime';

// ── Main component ─────────────────────────────────────────

export function LogsPage() {
  const intl = useIntl();
  const [tab, setTab] = useState<Tab>('history');

  const tabs: TabItem[] = [
    {
      id: 'history',
      label: intl.formatMessage({ id: 'logs.tab.history' }),
      icon: History,
    },
    {
      id: 'realtime',
      label: intl.formatMessage({ id: 'logs.tab.realtime' }),
      icon: Radio,
    },
  ];

  return (
    <Page wide className="flex h-full flex-col">
      <PageHeader
        icon={FileText}
        title={intl.formatMessage({ id: 'nav.logs' })}
        subtitle={intl.formatMessage({ id: 'logs.subtitle' })}
      />

      <Tabs items={tabs} value={tab} onChange={(id) => setTab(id as Tab)} />

      {tab === 'history' ? <HistoryTab /> : <RealtimeTab />}
    </Page>
  );
}

// ── History tab (unified audit events from JSONL sources) ─────

type SeverityFilter = 'all' | 'info' | 'warning' | 'critical';

const ALL_SOURCES: UnifiedAuditSource[] = [
  'security',
  'tool_call',
  'channel_failure',
  'feedback',
];

function HistoryTab() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [events, setEvents] = useState<UnifiedAuditEvent[]>([]);
  const [sourceCounts, setSourceCounts] = useState<Record<UnifiedAuditSource, number>>({
    security: 0,
    tool_call: 0,
    channel_failure: 0,
    feedback: 0,
  });
  const [loading, setLoading] = useState(false);
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  // null = all sources; otherwise whitelist subset
  const [selectedSources, setSelectedSources] = useState<UnifiedAuditSource[] | null>(null);
  const [severityFilter, setSeverityFilter] = useState<SeverityFilter>('all');

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;
    setLoading(true);
    const params: {
      limit: number;
      sources?: UnifiedAuditSource[];
      severity_filter?: 'info' | 'warning' | 'critical';
    } = { limit: 300 };
    if (selectedSources !== null) params.sources = selectedSources;
    if (severityFilter !== 'all') params.severity_filter = severityFilter;

    api.audit
      .unifiedLog(params)
      .then((res) => {
        if (cancelled) return;
        setEvents(res?.events ?? []);
        setSourceCounts(
          res?.source_counts ?? {
            security: 0,
            tool_call: 0,
            channel_failure: 0,
            feedback: 0,
          },
        );
      })
      .catch((e) => {
        if (cancelled) return;
        setEvents([]);
        toast.error(
          intl.formatMessage(
            { id: 'toast.error.loadFailed' },
            { message: formatError(e) },
          ),
        );
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [connectionState, selectedSources, severityFilter, intl]);

  const toggleSource = (src: UnifiedAuditSource) => {
    setSelectedSources((prev) => {
      if (prev === null) {
        // Going from "all" to a single selection
        return [src];
      }
      if (prev.includes(src)) {
        const next = prev.filter((s) => s !== src);
        return next.length === 0 ? null : next;
      }
      const next = [...prev, src];
      // If all four selected, collapse back to "all" (null)
      return next.length === ALL_SOURCES.length ? null : next;
    });
  };

  const isSourceActive = (src: UnifiedAuditSource) =>
    selectedSources === null || selectedSources.includes(src);

  const totalCount =
    sourceCounts.security +
    sourceCounts.tool_call +
    sourceCounts.channel_failure +
    sourceCounts.feedback;

  return (
    <div className="flex flex-1 flex-col gap-4 overflow-hidden">
      {/* Filter bar */}
      <Toolbar>
        <div className="flex flex-wrap items-center gap-1.5">
          <SourceChip
            active={selectedSources === null}
            onClick={() => setSelectedSources(null)}
            label={intl.formatMessage({ id: 'logs.filter.source.all' })}
            count={totalCount}
          />
          {ALL_SOURCES.map((src) => (
            <SourceChip
              key={src}
              active={selectedSources !== null && isSourceActive(src)}
              onClick={() => toggleSource(src)}
              label={intl.formatMessage({ id: `logs.filter.source.${src}` })}
              count={sourceCounts[src]}
            />
          ))}
        </div>

        <select
          value={severityFilter}
          onChange={(e) => setSeverityFilter(e.target.value as SeverityFilter)}
          className={cn(controlClass, 'w-auto')}
        >
          <option value="all">
            {intl.formatMessage({ id: 'logs.filter.severity.all' })}
          </option>
          <option value="info">info</option>
          <option value="warning">warning</option>
          <option value="critical">critical</option>
        </select>
      </Toolbar>

      {/* Body */}
      {loading ? (
        <HistoryLoadingSkeleton />
      ) : events.length === 0 ? (
        <Card className="flex-1" bodyClassName="flex h-full items-center justify-center">
          <EmptyState
            icon={Inbox}
            title={intl.formatMessage({ id: 'logs.empty.noMatch' })}
          />
        </Card>
      ) : (
        <div className="flex-1 space-y-2 overflow-y-auto pr-1">
          {events.map((evt, i) => {
            const key = `${evt.timestamp}-${evt.source}-${i}`;
            return (
              <AuditRow
                key={key}
                event={evt}
                expanded={expandedKey === key}
                onToggle={() => setExpandedKey(expandedKey === key ? null : key)}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

function SourceChip({
  active,
  onClick,
  label,
  count,
}: {
  readonly active: boolean;
  readonly onClick: () => void;
  readonly label: string;
  readonly count: number;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs font-medium transition-colors',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
        active
          ? 'border-amber-400 bg-amber-500/12 text-amber-800 dark:border-amber-500/50 dark:text-amber-300'
          : 'border-[var(--panel-border)] bg-[var(--panel-fill)] text-stone-600 hover:bg-[var(--panel-fill-hover)] dark:text-stone-400',
      )}
    >
      <span>{label}</span>
      <span
        className={cn(
          'rounded-full px-1.5 py-0.5 text-[10px] font-semibold tabular-nums',
          active
            ? 'bg-amber-200 text-amber-900 dark:bg-amber-800/60 dark:text-amber-200'
            : 'bg-stone-500/12 text-stone-500 dark:text-stone-400',
        )}
      >
        {count}
      </span>
    </button>
  );
}

function HistoryLoadingSkeleton() {
  return (
    <div className="flex-1 space-y-2 overflow-hidden">
      {Array.from({ length: 6 }).map((_, i) => (
        <div
          key={i}
          className="panel h-16 animate-pulse"
        />
      ))}
    </div>
  );
}

// ── Audit row ──────────────────────────────────────────────

const severityBorder: Record<UnifiedAuditEvent['severity'], string> = {
  info: 'border-l-emerald-400 dark:border-l-emerald-500',
  warning: 'border-l-amber-400 dark:border-l-amber-500',
  critical: 'border-l-rose-400 dark:border-l-rose-500',
};

function AuditRow({
  event,
  expanded,
  onToggle,
}: {
  readonly event: UnifiedAuditEvent;
  readonly expanded: boolean;
  readonly onToggle: () => void;
}) {
  const intl = useIntl();
  const time = (() => {
    try {
      return new Date(event.timestamp).toLocaleString('zh-TW', {
        hour12: false,
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
      });
    } catch {
      return event.timestamp;
    }
  })();

  const sourceLabel = intl.formatMessage({
    id: `logs.filter.source.${event.source}`,
  });

  return (
    <Card
      interactive
      onClick={onToggle}
      className={cn('border-l-4', severityBorder[event.severity])}
      bodyClassName="p-4"
    >
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone="neutral">{sourceLabel}</Badge>
        <span className="font-mono text-xs text-stone-500 dark:text-stone-400">
          {event.event_type}
        </span>
        {event.agent_id && (
          <span className="text-xs font-medium text-amber-600 dark:text-amber-400">
            {event.agent_id}
          </span>
        )}
        <span className="ml-auto text-xs text-stone-400 dark:text-stone-500">
          {time}
        </span>
      </div>

      {event.summary && (
        <p className="mt-2 whitespace-normal break-words text-sm text-stone-700 dark:text-stone-200">
          {event.summary}
        </p>
      )}

      {expanded && event.details && Object.keys(event.details).length > 0 && (
        <pre className="mt-3 overflow-x-auto rounded-lg bg-stone-500/8 p-3 text-xs text-stone-700 dark:bg-white/5 dark:text-stone-300">
          {JSON.stringify(event.details, null, 2)}
        </pre>
      )}
    </Card>
  );
}

// ── Realtime tab (WebSocket stream) ────────────────────────

function RealtimeTab() {
  const intl = useIntl();
  const {
    entries,
    paused,
    filter,
    subscribe,
    unsubscribe,
    togglePause,
    setFilter,
    clear,
  } = useLogsStore();
  // Show all agents by default — no agent filter
  const filteredEntries = useMemo(
    () => selectFilteredEntries({ entries, filter: { ...filter, agentId: null } }),
    [entries, filter],
  );
  const { fetchAgents } = useAgentsStore();
  const connectionState = useConnectionStore((s) => s.state);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchAgents();
    subscribe();
    return () => {
      unsubscribe();
    };
  }, [connectionState, fetchAgents, subscribe, unsubscribe]);

  // Auto-scroll to bottom when new entries arrive (unless paused)
  useEffect(() => {
    if (!paused && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [filteredEntries, paused]);

  const levels = ['trace', 'debug', 'info', 'warn', 'error'];

  return (
    <div className="flex flex-1 flex-col gap-4 overflow-hidden">
      {/* Filter bar */}
      <Toolbar
        search={filter.keyword}
        onSearchChange={(v) => setFilter({ keyword: v })}
        searchPlaceholder="Filter..."
      >
        {/* Level select */}
        <select
          value={filter.level ?? ''}
          onChange={(e) => setFilter({ level: e.target.value || null })}
          className={cn(controlClass, 'w-auto')}
        >
          <option value="">
            {intl.formatMessage({ id: 'logs.filter.all' })}
          </option>
          {levels.map((level) => (
            <option key={level} value={level}>
              {level.toUpperCase()}
            </option>
          ))}
        </select>

        {/* Pause / Resume */}
        <Button
          variant={paused ? 'secondary' : 'primary'}
          size="sm"
          icon={paused ? Play : Pause}
          onClick={togglePause}
        >
          {paused
            ? intl.formatMessage({ id: 'logs.resume' })
            : intl.formatMessage({ id: 'logs.pause' })}
        </Button>

        {/* Clear */}
        <Button variant="ghost" size="sm" icon={Trash2} onClick={clear}>
          {intl.formatMessage({ id: 'logs.clear' })}
        </Button>
      </Toolbar>

      {/* Log entries */}
      <div
        ref={listRef}
        className="flex-1 overflow-y-auto rounded-xl border border-[var(--panel-border)] bg-stone-950 p-1"
      >
        {filteredEntries.length === 0 ? (
          <div className="flex items-center justify-center py-16 text-stone-500">
            <p>{intl.formatMessage({ id: 'common.noData' })}</p>
          </div>
        ) : (
          <div className="space-y-px">
            {filteredEntries.map((entry, i) => (
              <div
                key={`${entry.target}-${entry.timestamp}-${i}`}
                className={cn(
                  'flex items-start gap-3 rounded px-3 py-1.5 font-mono text-xs',
                  levelBg[entry.level] ?? 'bg-transparent',
                )}
              >
                {/* Timestamp */}
                <span className="shrink-0 text-stone-500">
                  {formatTimestamp(entry.timestamp)}
                </span>

                {/* Level badge */}
                <span
                  className={cn(
                    'w-12 shrink-0 text-right font-semibold uppercase',
                    levelStyles[entry.level] ?? 'text-stone-400',
                  )}
                >
                  {entry.level}
                </span>

                {/* Agent ID — always shown */}
                {entry.agent_id ? (
                  <span className="shrink-0 rounded bg-amber-900/30 px-1.5 py-0.5 text-amber-400">
                    {entry.agent_id}
                  </span>
                ) : (
                  <span className="shrink-0 rounded bg-stone-800 px-1.5 py-0.5 text-stone-500">
                    system
                  </span>
                )}

                {/* Target */}
                <span className="shrink-0 text-stone-400 dark:text-stone-500">
                  {entry.target}
                </span>

                {/* Message */}
                <span
                  className={cn(
                    'flex-1 break-all',
                    entry.level === 'error'
                      ? 'text-rose-300'
                      : entry.level === 'warn'
                        ? 'text-amber-300'
                        : 'text-stone-300',
                  )}
                >
                  {entry.message}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Helpers ────────────────────────────────────────────────

function formatTimestamp(ts: string): string {
  try {
    const date = new Date(ts);
    return date.toLocaleTimeString('zh-TW', {
      hour12: false,
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      fractionalSecondDigits: 3,
    });
  } catch {
    return ts;
  }
}
