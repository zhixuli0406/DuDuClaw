import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Inbox } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type UnifiedAuditEvent, type UnifiedAuditSource } from '@/lib/api';
import { Badge, Empty, Segmented, Skeleton, type SegmentedOption } from '@/components/mds';
import { useDevPanelIdJump } from './id-jump';

/** Tail-poll interval — short enough to feel "live" for a developer keeping
 *  the panel open during an incident, long enough not to hammer the gateway
 *  from an always-mountable global overlay. */
const POLL_MS = 8000;
const ROW_LIMIT = 100;

const ALL_SOURCES: UnifiedAuditSource[] = ['security', 'tool_call', 'channel_failure', 'feedback'];

type SeverityFilter = 'all' | 'info' | 'warning' | 'critical';

const severityBorder: Record<UnifiedAuditEvent['severity'], string> = {
  info: 'border-l-emerald-400 dark:border-l-emerald-500',
  warning: 'border-l-amber-400 dark:border-l-amber-500',
  critical: 'border-l-rose-400 dark:border-l-rose-500',
};

/**
 * 事件流 tab — `audit.unified_log` tail, polled (spec allows either polling or
 * the raw-log `logs.entry` WS stream; polling is used here because this is
 * structured business-event granularity, not tracing-log granularity, and a
 * global overlay that mounts/unmounts with panel visibility is a bad fit for
 * a standing WS subscription). Multi-select source filter + expandable raw
 * JSON per row, same interaction shape as `LogsPage`'s history tab, compacted
 * for the panel's smaller footprint.
 */
export function DevPanelEventsTab() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const { jumpToAgent } = useDevPanelIdJump();

  const [events, setEvents] = useState<UnifiedAuditEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  const [selectedSources, setSelectedSources] = useState<UnifiedAuditSource[] | null>(null);
  const [severityFilter, setSeverityFilter] = useState<SeverityFilter>('all');

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;

    const load = (isFirst: boolean) => {
      if (isFirst) setLoading(true);
      const params: {
        limit: number;
        sources?: UnifiedAuditSource[];
        severity_filter?: 'info' | 'warning' | 'critical';
      } = { limit: ROW_LIMIT };
      if (selectedSources !== null) params.sources = selectedSources;
      if (severityFilter !== 'all') params.severity_filter = severityFilter;

      api.audit
        .unifiedLog(params)
        .then((res) => {
          if (cancelled) return;
          setEvents(res?.events ?? []);
          setFailed(false);
        })
        .catch(() => {
          if (cancelled) return;
          if (isFirst) setFailed(true);
        })
        .finally(() => {
          if (!cancelled && isFirst) setLoading(false);
        });
    };

    load(true);
    const id = setInterval(() => load(false), POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [connectionState, selectedSources, severityFilter]);

  const toggleSource = (src: UnifiedAuditSource) => {
    setSelectedSources((prev) => {
      if (prev === null) return [src];
      if (prev.includes(src)) {
        const next = prev.filter((s) => s !== src);
        return next.length === 0 ? null : next;
      }
      const next = [...prev, src];
      return next.length === ALL_SOURCES.length ? null : next;
    });
  };

  const severityOptions: SegmentedOption<SeverityFilter>[] = [
    { value: 'all', label: intl.formatMessage({ id: 'logs.filter.severity.all' }) },
    { value: 'info', label: 'info' },
    { value: 'warning', label: 'warning' },
    { value: 'critical', label: 'critical' },
  ];

  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex flex-wrap items-center gap-1.5">
        <FilterChip
          active={selectedSources === null}
          onClick={() => setSelectedSources(null)}
          label={intl.formatMessage({ id: 'logs.filter.source.all' })}
        />
        {ALL_SOURCES.map((src) => (
          <FilterChip
            key={src}
            active={selectedSources !== null && selectedSources.includes(src)}
            onClick={() => toggleSource(src)}
            label={intl.formatMessage({ id: `logs.filter.source.${src}` })}
          />
        ))}
        <Segmented
          className="ml-auto"
          value={severityFilter}
          onValueChange={setSeverityFilter}
          options={severityOptions}
        />
      </div>

      {loading ? (
        <div className="space-y-1.5">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded-lg" />
          ))}
        </div>
      ) : failed ? (
        <p className="py-6 text-center text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'devpanel.events.loadError' })}
        </p>
      ) : events.length === 0 ? (
        <Empty
          icon={Inbox}
          title={intl.formatMessage({ id: 'devpanel.events.empty.title' })}
          description={intl.formatMessage({ id: 'devpanel.events.empty.desc' })}
          className="py-8"
        />
      ) : (
        <div className="min-h-0 flex-1 space-y-1 overflow-y-auto">
          {events.map((evt, i) => {
            const key = `${evt.timestamp}-${evt.source}-${i}`;
            return (
              <EventRow
                key={key}
                event={evt}
                expanded={expandedKey === key}
                onToggle={() => setExpandedKey(expandedKey === key ? null : key)}
                onAgentClick={jumpToAgent}
              />
            );
          })}
        </div>
      )}

      <p className="shrink-0 text-[11px] text-muted-foreground">
        {intl.formatMessage({ id: 'devpanel.events.autoRefresh' }, { seconds: POLL_MS / 1000 })}
      </p>
    </div>
  );
}

function FilterChip({
  active,
  onClick,
  label,
}: {
  readonly active: boolean;
  readonly onClick: () => void;
  readonly label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'rounded-full border px-2 py-0.5 text-[11px] font-medium transition-colors',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
        active
          ? 'border-brand bg-brand/10 text-brand'
          : 'border-surface-border bg-surface text-muted-foreground hover:bg-surface-hover',
      )}
    >
      {label}
    </button>
  );
}

function EventRow({
  event,
  expanded,
  onToggle,
  onAgentClick,
}: {
  readonly event: UnifiedAuditEvent;
  readonly expanded: boolean;
  readonly onToggle: () => void;
  readonly onAgentClick: (agentId: string) => void;
}) {
  const intl = useIntl();
  const sourceLabel = intl.formatMessage({ id: `logs.filter.source.${event.source}` });
  const time = (() => {
    try {
      return new Date(event.timestamp).toLocaleTimeString('zh-TW', {
        hour12: false,
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
      });
    } catch {
      return event.timestamp;
    }
  })();

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onToggle}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onToggle();
        }
      }}
      className={cn(
        'cursor-pointer rounded-lg border border-l-4 border-surface-border bg-surface px-2.5 py-1.5 text-xs',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
        severityBorder[event.severity],
      )}
    >
      <div className="flex flex-wrap items-center gap-1.5">
        <Badge variant="ghost" className="h-4 px-1.5 text-[10px]">
          {sourceLabel}
        </Badge>
        <span className="font-mono text-[11px] text-muted-foreground">{event.event_type}</span>
        {event.agent_id && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onAgentClick(event.agent_id);
            }}
            className="rounded font-mono text-[11px] font-medium text-brand hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            {event.agent_id}
          </button>
        )}
        <span className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground">{time}</span>
      </div>
      {event.summary && (
        <p className="mt-1 whitespace-normal break-words text-foreground">{event.summary}</p>
      )}
      {expanded && event.details && Object.keys(event.details).length > 0 && (
        <pre className="mt-1.5 overflow-x-auto rounded-md bg-muted p-2 font-mono text-[10px] text-muted-foreground">
          {JSON.stringify(event.details, null, 2)}
        </pre>
      )}
    </div>
  );
}
