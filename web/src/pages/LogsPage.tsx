import { useEffect, useRef, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useLogsStore, selectFilteredEntries } from '@/stores/logs-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type AuditEvent } from '@/lib/api';
import {
  Pause,
  Play,
  Trash2,
  Search,
  History,
  Radio,
  Shield,
  AlertTriangle,
  FileWarning,
  User,
  Hash,
} from 'lucide-react';

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

  return (
    <div className="flex h-full flex-col space-y-4">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'logs.title' })}
      </h2>

      {/* Tab bar */}
      <div className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
        <TabButton
          active={tab === 'history'}
          onClick={() => setTab('history')}
          icon={<History className="h-4 w-4" />}
          label={intl.formatMessage({ id: 'logs.tab.history' })}
        />
        <TabButton
          active={tab === 'realtime'}
          onClick={() => setTab('realtime')}
          icon={<Radio className="h-4 w-4" />}
          label={intl.formatMessage({ id: 'logs.tab.realtime' })}
        />
      </div>

      {tab === 'history' ? <HistoryTab /> : <RealtimeTab />}
    </div>
  );
}

// ── Tab button ─────────────────────────────────────────────

function TabButton({
  active,
  onClick,
  icon,
  label,
}: {
  readonly active: boolean;
  readonly onClick: () => void;
  readonly icon: React.ReactNode;
  readonly label: string;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        'inline-flex flex-1 items-center justify-center gap-2 rounded-md px-4 py-2 text-sm font-medium transition-colors',
        active
          ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
          : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200',
      )}
    >
      {icon}
      {label}
    </button>
  );
}

// ── History tab (audit events from JSONL) ──────────────────

function HistoryTab() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [events, setEvents] = useState<AuditEvent[]>([]);
  const [loading, setLoading] = useState(false);
  const [expandedIdx, setExpandedIdx] = useState<number | null>(null);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    api.security
      .auditLog(100)
      .then((res) => setEvents(res?.events ?? []))
      .catch(() => setEvents([]))
      .finally(() => setLoading(false));
  }, [connectionState]);

  if (loading) {
    return (
      <div className="flex flex-1 items-center justify-center text-stone-400">
        {intl.formatMessage({ id: 'common.loading' })}
      </div>
    );
  }

  if (events.length === 0) {
    return (
      <div className="flex flex-1 items-center justify-center text-stone-400">
        {intl.formatMessage({ id: 'logs.history.empty' })}
      </div>
    );
  }

  return (
    <div className="flex-1 space-y-2 overflow-y-auto">
      {events.map((evt, i) => (
        <AuditRow
          key={`${evt.timestamp}-${i}`}
          event={evt}
          expanded={expandedIdx === i}
          onToggle={() => setExpandedIdx(expandedIdx === i ? null : i)}
        />
      ))}
    </div>
  );
}

// ── Audit row ──────────────────────────────────────────────

const severityStyles: Record<string, string> = {
  info: 'text-blue-500',
  warning: 'text-amber-500',
  critical: 'text-rose-500',
};

function AuditRow({
  event,
  expanded,
  onToggle,
}: {
  readonly event: AuditEvent;
  readonly expanded: boolean;
  readonly onToggle: () => void;
}) {
  const SevIcon =
    event.severity === 'critical'
      ? AlertTriangle
      : event.severity === 'warning'
        ? FileWarning
        : Shield;

  const time = new Date(event.timestamp).toLocaleString('zh-TW', {
    hour12: false,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });

  const userId = (event.details as Record<string, unknown>)?.user_id as string | undefined;
  const channel = (event.details as Record<string, unknown>)?.channel as string | undefined;
  const scope = (event.details as Record<string, unknown>)?.scope as string | undefined;

  return (
    <div
      className="cursor-pointer rounded-xl border border-stone-200 bg-white p-4 transition-colors hover:bg-stone-50 dark:border-stone-800 dark:bg-stone-900 dark:hover:bg-stone-800/70"
      onClick={onToggle}
    >
      <div className="flex items-start gap-3">
        <SevIcon
          className={`mt-0.5 h-4 w-4 shrink-0 ${severityStyles[event.severity] ?? 'text-stone-400'}`}
        />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium text-stone-900 dark:text-stone-100">
              {event.event_type}
            </span>
            {event.agent_id && (
              <span className="rounded bg-amber-100 px-1.5 py-0.5 text-xs font-medium text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">
                {event.agent_id}
              </span>
            )}
            {(channel ?? scope) && (
              <span className="inline-flex items-center gap-1 rounded bg-blue-100 px-1.5 py-0.5 text-xs text-blue-700 dark:bg-blue-900/30 dark:text-blue-400">
                <Hash className="h-3 w-3" />
                {channel ?? scope}
              </span>
            )}
            {userId && (
              <span className="inline-flex items-center gap-1 rounded bg-violet-100 px-1.5 py-0.5 text-xs text-violet-700 dark:bg-violet-900/30 dark:text-violet-400">
                <User className="h-3 w-3" />
                {userId}
              </span>
            )}
          </div>
          <p className="mt-1 text-xs text-stone-500 dark:text-stone-400">{time}</p>
        </div>
      </div>

      {expanded && event.details && Object.keys(event.details).length > 0 && (
        <pre className="mt-3 overflow-x-auto rounded-lg bg-stone-100 p-3 text-xs text-stone-700 dark:bg-stone-800 dark:text-stone-300">
          {JSON.stringify(event.details, null, 2)}
        </pre>
      )}
    </div>
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
    <>
      {/* Filter bar */}
      <div className="flex flex-wrap items-center gap-3">
        {/* Level select */}
        <select
          value={filter.level ?? ''}
          onChange={(e) => setFilter({ level: e.target.value || null })}
          className="rounded-lg border border-stone-200 bg-white px-3 py-2 text-sm text-stone-700 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300"
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

        {/* Keyword search */}
        <div className="relative min-w-[200px] flex-1">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="text"
            value={filter.keyword}
            onChange={(e) => setFilter({ keyword: e.target.value })}
            placeholder="Filter..."
            className="w-full rounded-lg border border-stone-200 bg-white py-2 pl-10 pr-4 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500"
          />
        </div>

        {/* Pause / Clear buttons */}
        <button
          onClick={togglePause}
          className={cn(
            'inline-flex items-center gap-1.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
            paused
              ? 'bg-emerald-100 text-emerald-700 hover:bg-emerald-200 dark:bg-emerald-900/30 dark:text-emerald-400'
              : 'bg-amber-100 text-amber-700 hover:bg-amber-200 dark:bg-amber-900/30 dark:text-amber-400',
          )}
        >
          {paused ? (
            <>
              <Play className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'logs.resume' })}
            </>
          ) : (
            <>
              <Pause className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'logs.pause' })}
            </>
          )}
        </button>

        <button
          onClick={clear}
          className="inline-flex items-center gap-1.5 rounded-lg bg-stone-100 px-3 py-2 text-sm font-medium text-stone-600 transition-colors hover:bg-stone-200 dark:bg-stone-800 dark:text-stone-400 dark:hover:bg-stone-700"
        >
          <Trash2 className="h-3.5 w-3.5" />
          {intl.formatMessage({ id: 'logs.clear' })}
        </button>
      </div>

      {/* Log entries */}
      <div
        ref={listRef}
        className="flex-1 overflow-y-auto rounded-xl border border-stone-200 bg-stone-950 p-1 dark:border-stone-800"
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
    </>
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
