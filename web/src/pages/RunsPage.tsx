import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  CheckCircle2,
  ChevronDown,
  ListChecks,
  ScrollText,
  User,
  Wrench,
  XCircle,
} from 'lucide-react';
import { api } from '@/lib/api';
import {
  cardsForEvents,
  isRunLive,
  relativeParts,
  runDurationSecs,
  runStatusMeta,
  type RunDetail,
  type RunStatus,
  type RunSummary,
} from '@/lib/run-transcript';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useDataScope, useVisibleAgents } from '@/lib/data-scope';
import {
  Page,
  PageHeader,
  Card,
  Badge,
  EmptyState,
  Skeleton,
  LiveBadge,
  Mono,
  CharacterAvatar,
  controlClass,
} from '@/components/ui';

/**
 * RunsPage (G12 run inspector) — 執行紀錄. Left: recent runs per AI staff
 * member; right: the selected run's transcript as chronological cards
 * (prose turns + collapsible tool-call cards). Everything shown derives from
 * persisted stores (session turns + MCP tool receipts) — the page states what
 * is NOT persisted instead of fabricating events.
 */

const LIST_REFRESH_MS = 30_000;
/** A live (running) run's transcript is re-fetched at this cadence. */
const LIVE_POLL_MS = 5_000;
/** "Close enough to the bottom" threshold for the auto-scroll pin. */
const BOTTOM_EPSILON_PX = 32;

/** Proper-noun channel names (not translated); "other" falls back to i18n. */
const CHANNEL_NAMES: Record<string, string> = {
  telegram: 'Telegram',
  discord: 'Discord',
  line: 'LINE',
  slack: 'Slack',
  whatsapp: 'WhatsApp',
  feishu: 'Feishu',
  googlechat: 'Google Chat',
  msteams: 'Teams',
  webchat: 'WebChat',
};

function StatusBadge({ status }: { status: RunStatus | string }) {
  const intl = useIntl();
  if (status === 'running') return <LiveBadge />;
  const meta = runStatusMeta(status);
  return (
    <Badge tone={status === 'completed' ? 'success' : 'neutral'} dot>
      {intl.formatMessage({ id: meta.labelId })}
    </Badge>
  );
}

export function RunsPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const scope = useDataScope();
  const visibleAgents = useVisibleAgents();

  const [agentFilter, setAgentFilter] = useState('');
  const [runs, setRuns] = useState<RunSummary[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  const [listLoaded, setListLoaded] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<RunDetail | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);

  // Non-admin scopes must query per agent (the gateway fails closed without
  // an agent_id) — default to the first AI staff member the viewer can see.
  const effectiveAgent =
    agentFilter || (scope !== 'all' ? (visibleAgents[0]?.name ?? '') : '');

  const agentName = useCallback(
    (id: string) => agents.find((a) => a.name === id)?.display_name || id,
    [agents],
  );

  const channelLabel = useCallback(
    (ch: string) =>
      CHANNEL_NAMES[ch] ??
      (ch === 'other'
        ? intl.formatMessage({ id: 'runs.channel.other' })
        : ch),
    [intl],
  );

  // ── Runs list: initial fetch + gentle refresh ─────────────
  const fetchRuns = useCallback(async () => {
    if (scope !== 'all' && !effectiveAgent) return; // nothing visible yet
    try {
      const res = await api.runs.list(
        effectiveAgent ? { agent_id: effectiveAgent } : {},
      );
      setRuns(res.runs);
      setListError(null);
    } catch (e) {
      setListError(String(e));
    } finally {
      setListLoaded(true);
    }
  }, [effectiveAgent, scope]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    if (agents.length === 0) void fetchAgents();
  }, [connectionState, agents.length, fetchAgents]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    void fetchRuns();
    const id = setInterval(() => void fetchRuns(), LIST_REFRESH_MS);
    return () => clearInterval(id);
  }, [connectionState, fetchRuns]);

  // ── Transcript: fetch on selection; poll while live ───────
  const fetchDetail = useCallback(async (runId: string, replace: boolean) => {
    if (replace) {
      setDetailLoading(true);
      setDetail(null);
    }
    try {
      const res = await api.runs.get(runId);
      setDetail(res);
      setDetailError(null);
    } catch (e) {
      setDetailError(String(e));
    } finally {
      setDetailLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!selectedId || connectionState !== 'authenticated') return;
    void fetchDetail(selectedId, true);
  }, [selectedId, connectionState, fetchDetail]);

  const detailIsLive = detail ? isRunLive(detail.run) : false;
  useEffect(() => {
    if (!selectedId || !detailIsLive || connectionState !== 'authenticated') return;
    const id = setInterval(() => void fetchDetail(selectedId, false), LIVE_POLL_MS);
    return () => clearInterval(id);
  }, [selectedId, detailIsLive, connectionState, fetchDetail]);

  // ── Auto-scroll: pinned to bottom only when already at bottom ──
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const atBottomRef = useRef(true);
  const onTranscriptScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    atBottomRef.current =
      el.scrollTop + el.clientHeight >= el.scrollHeight - BOTTOM_EPSILON_PX;
  };
  const cards = useMemo(
    () => (detail ? cardsForEvents(detail.events) : []),
    [detail],
  );
  useEffect(() => {
    const el = scrollRef.current;
    if (el && atBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [cards.length]);
  // A fresh selection always starts pinned.
  useEffect(() => {
    atBottomRef.current = true;
  }, [selectedId]);

  const nowMs = Date.now();
  const relLabel = (ts: string) => {
    const parts = relativeParts(ts, nowMs);
    return parts ? intl.formatRelativeTime(parts.value, parts.unit) : ts;
  };
  const fmtDuration = (secs: number) =>
    secs >= 60
      ? intl.formatMessage(
          { id: 'runs.duration.minSec' },
          { min: Math.floor(secs / 60), sec: secs % 60 },
        )
      : intl.formatMessage({ id: 'runs.duration.sec' }, { sec: secs });

  const showAllOption = scope === 'all';
  const duration = detail ? runDurationSecs(detail.run) : null;

  return (
    <Page wide>
      <PageHeader
        icon={ScrollText}
        title={intl.formatMessage({ id: 'nav.runs' })}
        subtitle={intl.formatMessage({ id: 'runs.subtitle' })}
        actions={
          <select
            aria-label={intl.formatMessage({ id: 'runs.filter.agent' })}
            className={`${controlClass} max-w-56`}
            value={agentFilter || effectiveAgent}
            onChange={(e) => {
              setAgentFilter(e.target.value);
              setSelectedId(null);
              setDetail(null);
            }}
          >
            {showAllOption && (
              <option value="">{intl.formatMessage({ id: 'runs.allAgents' })}</option>
            )}
            {visibleAgents.map((a) => (
              <option key={a.name} value={a.name}>
                {a.display_name || a.name}
              </option>
            ))}
          </select>
        }
      />

      <div className="grid gap-4 lg:grid-cols-[minmax(280px,340px)_1fr]">
        {/* ── Left: recent runs ── */}
        <Card padded={false}>
          {!listLoaded ? (
            <div className="space-y-3 p-4">
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-12 w-2/3" />
            </div>
          ) : listError ? (
            <div className="p-4">
              <EmptyState
                dudu={{ face: 'concerned' }}
                title={intl.formatMessage({ id: 'runs.error' })}
                hint={listError}
              />
            </div>
          ) : runs.length === 0 ? (
            <div className="p-4">
              <EmptyState
                dudu={{ face: 'sleep' }}
                title={intl.formatMessage({ id: 'runs.empty' })}
                hint={intl.formatMessage({ id: 'runs.empty.hint' })}
              />
            </div>
          ) : (
            <ul
              className="max-h-[70vh] divide-y divide-[var(--panel-border)] overflow-y-auto"
              aria-label={intl.formatMessage({ id: 'runs.list.aria' })}
            >
              {runs.map((r) => {
                const selected = r.id === selectedId;
                return (
                  <li key={r.id}>
                    <button
                      type="button"
                      onClick={() => setSelectedId(r.id)}
                      aria-current={selected ? 'true' : undefined}
                      className={`flex w-full items-start gap-3 px-4 py-3 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-amber-500/50 ${
                        selected
                          ? 'bg-amber-500/10'
                          : 'hover:bg-stone-500/5'
                      }`}
                    >
                      <CharacterAvatar
                        agentId={r.agent_id}
                        name={agentName(r.agent_id)}
                        size={32}
                      />
                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-2">
                          <span className="truncate text-sm font-medium text-stone-800 dark:text-stone-100">
                            {agentName(r.agent_id)}
                          </span>
                          <StatusBadge status={r.status} />
                        </span>
                        <span className="mt-0.5 block truncate text-xs text-stone-500 dark:text-stone-400">
                          {r.preview ||
                            intl.formatMessage({ id: 'runs.preview.empty' })}
                        </span>
                        <span className="mt-1 flex items-center gap-2 text-xs text-stone-400 dark:text-stone-500">
                          <span>{channelLabel(r.channel)}</span>
                          <span aria-hidden="true">·</span>
                          <span className="tabular-nums">{relLabel(r.started_at)}</span>
                          {r.step_count > 0 && (
                            <>
                              <span aria-hidden="true">·</span>
                              <span className="tabular-nums">
                                {intl.formatMessage(
                                  { id: 'runs.steps' },
                                  { count: r.step_count },
                                )}
                              </span>
                            </>
                          )}
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </Card>

        {/* ── Right: transcript ── */}
        <Card padded={false} bodyClassName="flex min-h-[420px] flex-col">
          {!selectedId ? (
            <div className="flex flex-1 items-center justify-center p-6">
              <EmptyState
                dudu={{ face: 'curious' }}
                title={intl.formatMessage({ id: 'runs.select' })}
                hint={intl.formatMessage({ id: 'runs.select.hint' })}
              />
            </div>
          ) : detailLoading ? (
            <div className="space-y-3 p-5">
              <Skeleton className="h-8 w-1/2" />
              <Skeleton className="h-20 w-full" />
              <Skeleton className="h-12 w-full" />
            </div>
          ) : detailError ? (
            <div className="p-6">
              <EmptyState
                dudu={{ face: 'concerned' }}
                title={intl.formatMessage({ id: 'runs.error' })}
                hint={detailError}
              />
            </div>
          ) : detail ? (
            <>
              {/* Transcript header */}
              <div className="flex flex-wrap items-center gap-3 border-b border-[var(--panel-border)] px-5 py-4">
                <CharacterAvatar
                  agentId={detail.run.agent_id}
                  name={agentName(detail.run.agent_id)}
                  size={36}
                />
                <div className="min-w-0">
                  <p className="truncate text-sm font-semibold text-stone-800 dark:text-stone-100">
                    {agentName(detail.run.agent_id)}
                  </p>
                  <p className="text-xs text-stone-500 dark:text-stone-400">
                    {channelLabel(detail.run.channel)}
                    <span aria-hidden="true"> · </span>
                    {intl.formatDate(detail.run.started_at, {
                      month: 'numeric',
                      day: 'numeric',
                    })}{' '}
                    {intl.formatTime(detail.run.started_at, {
                      hour: '2-digit',
                      minute: '2-digit',
                    })}
                    {duration !== null && (
                      <>
                        <span aria-hidden="true"> · </span>
                        <span className="tabular-nums">{fmtDuration(duration)}</span>
                      </>
                    )}
                  </p>
                </div>
                <div className="ml-auto">
                  <StatusBadge status={detail.run.status} />
                </div>
              </div>

              {/* Event cards */}
              <div
                ref={scrollRef}
                onScroll={onTranscriptScroll}
                className="max-h-[62vh] flex-1 space-y-3 overflow-y-auto px-5 py-4"
              >
                {cards.length === 0 && (
                  <p className="py-8 text-center text-sm text-stone-400">
                    {intl.formatMessage({ id: 'runs.transcript.empty' })}
                  </p>
                )}
                {cards.map((c, i) =>
                  // CLI-native tool step (run_steps.db) — same collapsed card
                  // pattern as MCP receipts, but no ok/fail icon: the step
                  // store persists starts only, and an outcome must never be
                  // invented.
                  c.type === 'step' ? (
                    <details
                      key={i}
                      className="group ml-8 rounded-xl border border-[var(--panel-border)] bg-stone-500/[0.03]"
                    >
                      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 [&::-webkit-details-marker]:hidden">
                        <Wrench
                          className="h-3.5 w-3.5 shrink-0 text-stone-400"
                          aria-hidden="true"
                        />
                        <Mono className="shrink-0 text-xs">{c.tool}</Mono>
                        <span className="min-w-0 flex-1 truncate text-xs text-stone-500 dark:text-stone-400">
                          {c.preview}
                        </span>
                        <span className="ml-auto shrink-0 text-xs text-stone-400 tabular-nums">
                          {intl.formatTime(c.ts, { hour: '2-digit', minute: '2-digit' })}
                        </span>
                        <ChevronDown
                          className="h-3.5 w-3.5 shrink-0 text-stone-400 transition-transform group-open:rotate-180"
                          aria-hidden="true"
                        />
                      </summary>
                      <div className="border-t border-[var(--panel-border)] px-3 py-2">
                        <pre className="overflow-x-auto text-xs whitespace-pre-wrap break-words text-stone-600 dark:text-stone-300">
                          {c.preview ||
                            intl.formatMessage({ id: 'runs.tool.noArgs' })}
                        </pre>
                      </div>
                    </details>
                  ) : c.type === 'todo' ? (
                    // TodoWrite board snapshot — collapsed card whose body is
                    // the already-rendered progress board.
                    <details
                      key={i}
                      className="group ml-8 rounded-xl border border-[var(--panel-border)] bg-stone-500/[0.03]"
                    >
                      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 [&::-webkit-details-marker]:hidden">
                        <ListChecks
                          className="h-3.5 w-3.5 shrink-0 text-stone-400"
                          aria-hidden="true"
                        />
                        <span className="min-w-0 flex-1 truncate text-xs text-stone-500 dark:text-stone-400">
                          {intl.formatMessage(
                            { id: 'runs.todo.snapshot' },
                            { label: c.label },
                          )}
                        </span>
                        <span className="ml-auto shrink-0 text-xs text-stone-400 tabular-nums">
                          {intl.formatTime(c.ts, { hour: '2-digit', minute: '2-digit' })}
                        </span>
                        <ChevronDown
                          className="h-3.5 w-3.5 shrink-0 text-stone-400 transition-transform group-open:rotate-180"
                          aria-hidden="true"
                        />
                      </summary>
                      <div className="border-t border-[var(--panel-border)] px-3 py-2">
                        <pre className="overflow-x-auto text-xs whitespace-pre-wrap break-words text-stone-600 dark:text-stone-300">
                          {c.preview}
                        </pre>
                      </div>
                    </details>
                  ) : c.type === 'prose' ? (
                    <div key={i} className="flex items-start gap-2.5">
                      {c.role === 'user' ? (
                        <span
                          aria-hidden="true"
                          className="mt-1 flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-stone-500/10"
                        >
                          <User className="h-3.5 w-3.5 text-stone-500 dark:text-stone-400" />
                        </span>
                      ) : (
                        <CharacterAvatar
                          agentId={detail.run.agent_id}
                          name={agentName(detail.run.agent_id)}
                          size={24}
                        />
                      )}
                      <div className="min-w-0 flex-1">
                        <p className="mb-0.5 text-xs text-stone-400 dark:text-stone-500">
                          {c.role === 'user'
                            ? intl.formatMessage({ id: 'runs.role.user' })
                            : agentName(detail.run.agent_id)}
                          <span aria-hidden="true"> · </span>
                          <span className="tabular-nums">
                            {intl.formatTime(c.ts, { hour: '2-digit', minute: '2-digit' })}
                          </span>
                        </p>
                        <div className="rounded-2xl bg-stone-500/5 px-4 py-3 text-sm whitespace-pre-wrap break-words text-stone-700 dark:text-stone-200">
                          {c.text}
                        </div>
                      </div>
                    </div>
                  ) : (
                    // Collapsible tool card — collapsed by default.
                    <details
                      key={i}
                      className="group ml-8 rounded-xl border border-[var(--panel-border)] bg-stone-500/[0.03]"
                    >
                      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 [&::-webkit-details-marker]:hidden">
                        <Wrench
                          className="h-3.5 w-3.5 shrink-0 text-stone-400"
                          aria-hidden="true"
                        />
                        <Mono className="shrink-0 text-xs">{c.tool}</Mono>
                        {c.ok ? (
                          <CheckCircle2
                            className="h-3.5 w-3.5 shrink-0 text-emerald-500"
                            aria-label={intl.formatMessage({ id: 'runs.tool.ok' })}
                          />
                        ) : (
                          <XCircle
                            className="h-3.5 w-3.5 shrink-0 text-rose-500"
                            aria-label={intl.formatMessage({ id: 'runs.tool.failed' })}
                          />
                        )}
                        <span className="min-w-0 flex-1 truncate text-xs text-stone-500 dark:text-stone-400">
                          {c.preview}
                        </span>
                        <span className="ml-auto shrink-0 text-xs text-stone-400 tabular-nums">
                          {intl.formatTime(c.ts, { hour: '2-digit', minute: '2-digit' })}
                        </span>
                        <ChevronDown
                          className="h-3.5 w-3.5 shrink-0 text-stone-400 transition-transform group-open:rotate-180"
                          aria-hidden="true"
                        />
                      </summary>
                      <div className="border-t border-[var(--panel-border)] px-3 py-2">
                        <pre className="overflow-x-auto text-xs whitespace-pre-wrap break-words text-stone-600 dark:text-stone-300">
                          {c.preview ||
                            intl.formatMessage({ id: 'runs.tool.noArgs' })}
                        </pre>
                      </div>
                    </details>
                  ),
                )}
              </div>

              {/* Honest coverage note — what is not recorded. */}
              <p className="border-t border-[var(--panel-border)] px-5 py-3 text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'runs.transcript.note' })}
              </p>
            </>
          ) : null}
        </Card>
      </div>
    </Page>
  );
}
