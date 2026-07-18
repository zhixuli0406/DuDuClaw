import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  ArrowLeft,
  CheckCircle2,
  ChevronDown,
  ListChecks,
  ScrollText,
  User,
  Wrench,
  XCircle,
} from 'lucide-react';
import { api } from '@/lib/api';
import { cn } from '@/lib/utils';
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
  PageHeader,
  Badge,
  Empty,
  Skeleton,
  ActorAvatar,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
  useIsMobile,
} from '@/components/mds';

/**
 * RunsPage (G12 run inspector) — 執行紀錄. Left: recent runs per AI staff
 * member; right: the selected run's transcript as chronological cards
 * (prose turns + collapsible tool-step tree). Everything shown derives from
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

/** Status dot colour for the run list rows. */
function statusDotClass(status: RunStatus | string): string {
  if (status === 'running') return 'bg-brand';
  if (status === 'completed') return 'bg-success';
  return 'bg-muted-foreground';
}

function StatusBadge({ status }: { status: RunStatus | string }) {
  const intl = useIntl();
  if (status === 'running') {
    return (
      <Badge variant="secondary" className="gap-1.5">
        <span className="size-1.5 animate-pulse rounded-full bg-brand" aria-hidden />
        {intl.formatMessage({ id: 'runs.status.running' })}
      </Badge>
    );
  }
  const meta = runStatusMeta(status);
  return (
    <Badge variant="secondary" className="gap-1.5">
      <span className={cn('size-1.5 rounded-full', statusDotClass(status))} aria-hidden />
      {intl.formatMessage({ id: meta.labelId })}
    </Badge>
  );
}

export function RunsPage() {
  const intl = useIntl();
  const isMobile = useIsMobile();
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

  // ── Left column: header + run list ───────────────────────────
  const listColumn = (
    <div className="flex h-full min-h-0 flex-col">
      <PageHeader hideTrigger>
        <ScrollText className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.runs' })}</h1>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{runs.length}</span>
        <div className="ml-auto">
          <Select
            value={agentFilter || effectiveAgent}
            onValueChange={(v) => {
              setAgentFilter(String(v));
              setSelectedId(null);
              setDetail(null);
            }}
          >
            <SelectTrigger size="sm" className="max-w-44">
              <SelectValue
                aria-label={intl.formatMessage({ id: 'runs.filter.agent' })}
                placeholder={intl.formatMessage({ id: 'runs.allAgents' })}
              >
                {(agentFilter || effectiveAgent)
                  ? agentName(agentFilter || effectiveAgent)
                  : intl.formatMessage({ id: 'runs.allAgents' })}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              {showAllOption && (
                <SelectItem value="">{intl.formatMessage({ id: 'runs.allAgents' })}</SelectItem>
              )}
              {visibleAgents.map((a) => (
                <SelectItem key={a.name} value={a.name}>
                  {a.display_name || a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </PageHeader>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {!listLoaded ? (
          <div className="space-y-3 p-4">
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-12 w-2/3" />
          </div>
        ) : listError ? (
          <div className="p-4">
            <Empty
              icon={ScrollText}
              tone="destructive"
              title={intl.formatMessage({ id: 'runs.error' })}
              description={listError}
            />
          </div>
        ) : runs.length === 0 ? (
          <div className="p-4">
            <Empty
              icon={ScrollText}
              title={intl.formatMessage({ id: 'runs.empty' })}
              description={intl.formatMessage({ id: 'runs.empty.hint' })}
            />
          </div>
        ) : (
          <ul className="py-1" aria-label={intl.formatMessage({ id: 'runs.list.aria' })}>
            {runs.map((r) => {
              const selected = r.id === selectedId;
              return (
                <li key={r.id}>
                  <button
                    type="button"
                    onClick={() => setSelectedId(r.id)}
                    aria-current={selected ? 'true' : undefined}
                    className={cn(
                      'flex w-full items-start gap-3 px-4 py-3 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring/50',
                      selected ? 'bg-accent/30' : 'hover:bg-accent/40',
                    )}
                  >
                    <ActorAvatar actorType="agent" size="lg" name={agentName(r.agent_id)} />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-2">
                        <span className="truncate text-sm font-medium text-foreground">
                          {agentName(r.agent_id)}
                        </span>
                        <span className={cn('size-1.5 shrink-0 rounded-full', statusDotClass(r.status))} aria-hidden />
                      </span>
                      <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                        {r.preview || intl.formatMessage({ id: 'runs.preview.empty' })}
                      </span>
                      <span className="mt-1 flex items-center gap-2 text-xs text-muted-foreground/70">
                        <span>{channelLabel(r.channel)}</span>
                        <span aria-hidden="true">·</span>
                        <span className="tabular-nums">{relLabel(r.started_at)}</span>
                        {r.step_count > 0 && (
                          <>
                            <span aria-hidden="true">·</span>
                            <span className="tabular-nums">
                              {intl.formatMessage({ id: 'runs.steps' }, { count: r.step_count })}
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
      </div>
    </div>
  );

  // ── Right column: transcript ─────────────────────────────────
  const detailColumn = (
    <div className="flex h-full min-h-0 flex-col">
      {isMobile && selectedId && (
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-2">
          <button
            type="button"
            onClick={() => setSelectedId(null)}
            aria-label={intl.formatMessage({ id: 'common.back' })}
            className="grid size-7 place-items-center rounded-md text-muted-foreground outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <ArrowLeft className="size-4" />
          </button>
          {detail && (
            <span className="truncate text-sm font-medium">{agentName(detail.run.agent_id)}</span>
          )}
        </div>
      )}

      {!selectedId ? (
        <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
          <ScrollText className="size-10 text-muted-foreground/30" />
          <div>
            <p className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'runs.select' })}</p>
            <p className="mt-1 text-sm text-muted-foreground">{intl.formatMessage({ id: 'runs.select.hint' })}</p>
          </div>
        </div>
      ) : detailLoading ? (
        <div className="space-y-3 p-5">
          <Skeleton className="h-8 w-1/2" />
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-12 w-full" />
        </div>
      ) : detailError ? (
        <div className="p-6">
          <Empty
            icon={ScrollText}
            tone="destructive"
            title={intl.formatMessage({ id: 'runs.error' })}
            description={detailError}
          />
        </div>
      ) : detail ? (
        <>
          {/* Transcript session header */}
          <div className="flex flex-wrap items-center gap-3 border-b border-surface-border px-5 py-4">
            <ActorAvatar actorType="agent" size="lg" name={agentName(detail.run.agent_id)} />
            <div className="min-w-0">
              <p className="truncate text-sm font-medium text-foreground">
                {agentName(detail.run.agent_id)}
              </p>
              <p className="text-xs text-muted-foreground">
                {channelLabel(detail.run.channel)}
                <span aria-hidden="true"> · </span>
                {intl.formatDate(detail.run.started_at, { month: 'numeric', day: 'numeric' })}{' '}
                {intl.formatTime(detail.run.started_at, { hour: '2-digit', minute: '2-digit' })}
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
            className="min-h-0 flex-1 space-y-3 overflow-y-auto px-5 py-4"
          >
            {cards.length === 0 && (
              <p className="py-8 text-center text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'runs.transcript.empty' })}
              </p>
            )}
            {cards.map((c, i) =>
              c.type === 'step' ? (
                // CLI-native tool step (run_steps.db) — no ok/fail icon: the step
                // store persists starts only, an outcome must never be invented.
                <ToolStep key={i} tool={c.tool} preview={c.preview} ts={c.ts} />
              ) : c.type === 'todo' ? (
                // TodoWrite board snapshot — collapsed step whose body is the
                // already-rendered progress board.
                <ToolStep
                  key={i}
                  icon={ListChecks}
                  label={intl.formatMessage({ id: 'runs.todo.snapshot' }, { label: c.label })}
                  preview={c.preview}
                  ts={c.ts}
                />
              ) : c.type === 'prose' ? (
                <div
                  key={i}
                  className={cn('flex w-full items-end gap-2', c.role === 'user' ? 'justify-end' : 'justify-start')}
                >
                  {c.role !== 'user' && (
                    <ActorAvatar
                      actorType="agent"
                      size="md"
                      name={agentName(detail.run.agent_id)}
                      className="mb-0.5 shrink-0"
                    />
                  )}
                  <div className="min-w-0 max-w-[80%]">
                    <p className="mb-0.5 text-xs text-muted-foreground">
                      {c.role === 'user'
                        ? intl.formatMessage({ id: 'runs.role.user' })
                        : agentName(detail.run.agent_id)}
                      <span aria-hidden="true"> · </span>
                      <span className="tabular-nums">
                        {intl.formatTime(c.ts, { hour: '2-digit', minute: '2-digit' })}
                      </span>
                    </p>
                    <div
                      className={cn(
                        'rounded-xl px-3.5 py-2 text-sm leading-relaxed whitespace-pre-wrap break-words',
                        c.role === 'user'
                          ? 'bg-secondary text-secondary-foreground'
                          : 'bg-surface text-surface-foreground ring-1 ring-surface-border',
                      )}
                    >
                      {c.text}
                    </div>
                  </div>
                  {c.role === 'user' && (
                    <span
                      aria-hidden="true"
                      className="mb-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-muted ring-1 ring-surface-border"
                    >
                      <User className="size-3.5 text-muted-foreground" />
                    </span>
                  )}
                </div>
              ) : (
                // MCP tool receipt — collapsed step with ok/fail icon.
                <ToolStep
                  key={i}
                  tool={c.tool}
                  preview={c.preview}
                  ts={c.ts}
                  ok={c.ok}
                  noArgsLabel={intl.formatMessage({ id: 'runs.tool.noArgs' })}
                  okLabel={intl.formatMessage({ id: 'runs.tool.ok' })}
                  failLabel={intl.formatMessage({ id: 'runs.tool.failed' })}
                />
              ),
            )}
          </div>

          {/* Honest coverage note — what is not recorded. */}
          <p className="border-t border-surface-border px-5 py-3 text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'runs.transcript.note' })}
          </p>
        </>
      ) : null}
    </div>
  );

  return (
    <div className="-mx-4 -mt-4 flex min-h-0 flex-1 md:-mx-6 md:-mt-6 md:-mb-6">
      {isMobile ? (
        selectedId ? (
          detailColumn
        ) : (
          <div className="w-full">{listColumn}</div>
        )
      ) : (
        <ResizablePanelGroup orientation="horizontal" id="runs-split" className="h-full w-full">
          <ResizablePanel defaultSize={320} minSize={240} maxSize={480} className="border-r border-surface-border">
            {listColumn}
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel minSize="40">{detailColumn}</ResizablePanel>
        </ResizablePanelGroup>
      )}
    </div>
  );
}

/**
 * ToolStep — one collapsible tool/step/todo receipt, rendered as a step-tree
 * item (spec §task2: `border-l-2 border-border pl-3 text-xs`) rather than a
 * bordered card. `ok` (when defined) renders the pass/fail glyph; steps omit it.
 */
function ToolStep({
  tool,
  icon: Icon = Wrench,
  label,
  preview,
  ts,
  ok,
  noArgsLabel,
  okLabel,
  failLabel,
}: {
  tool?: string;
  icon?: typeof Wrench;
  label?: string;
  preview: string;
  ts: string;
  ok?: boolean;
  noArgsLabel?: string;
  okLabel?: string;
  failLabel?: string;
}) {
  const intl = useIntl();
  return (
    <details className="group ml-6 border-l-2 border-border pl-3 text-xs">
      <summary className="flex cursor-pointer list-none items-center gap-2 py-1 outline-none focus-visible:ring-2 focus-visible:ring-ring/50 [&::-webkit-details-marker]:hidden">
        <Icon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
        {tool && <span className="shrink-0 font-mono text-xs text-foreground">{tool}</span>}
        {ok !== undefined &&
          (ok ? (
            <CheckCircle2 className="size-3.5 shrink-0 text-success" aria-label={okLabel} />
          ) : (
            <XCircle className="size-3.5 shrink-0 text-destructive" aria-label={failLabel} />
          ))}
        <span className="min-w-0 flex-1 truncate text-muted-foreground">{label ?? preview}</span>
        <span className="ml-auto shrink-0 tabular-nums text-muted-foreground">
          {intl.formatTime(ts, { hour: '2-digit', minute: '2-digit' })}
        </span>
        <ChevronDown
          className="size-3.5 shrink-0 text-muted-foreground transition-transform group-open:rotate-180"
          aria-hidden="true"
        />
      </summary>
      <div className="pt-1 pb-2">
        <pre className="overflow-x-auto whitespace-pre-wrap break-words text-muted-foreground">
          {preview || noArgsLabel}
        </pre>
      </div>
    </details>
  );
}
