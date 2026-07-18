import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { History, Presentation, RefreshCw, MoreHorizontal } from 'lucide-react';
import { api, type CanvasGetResult } from '@/lib/api';
import { canvasSrcDoc, CANVAS_SANDBOX } from '@/lib/canvas-doc';
import { client } from '@/lib/ws-client';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useDataScope, useVisibleAgents } from '@/lib/data-scope';
import {
  PageHeader,
  Card,
  Badge,
  Button,
  Empty,
  Skeleton,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from '@/components/mds';

/**
 * CanvasPage (G15 Live Canvas) — 畫布. An AI staff member pushes an HTML
 * visual workspace (report, chart, table…) via the `canvas_push` MCP tool;
 * this page shows it live.
 *
 * Security: the HTML was ammonia-sanitized by the gateway at write time, and
 * is rendered here EXCLUSIVELY inside `<iframe srcdoc sandbox="">` — empty
 * sandbox = no scripts, no same-origin, no forms, no navigation. Do not
 * render canvas HTML any other way.
 *
 * Freshness: `canvas.updated` WS broadcasts trigger an immediate refetch;
 * a 30s poll is the fallback while the WS is degraded.
 */

const POLL_MS = 30_000;

export function CanvasPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const scope = useDataScope();
  const visibleAgents = useVisibleAgents();

  const [agentFilter, setAgentFilter] = useState('');
  /** null = live current version; a number = pinned history version. */
  const [viewSeq, setViewSeq] = useState<number | null>(null);
  const [result, setResult] = useState<CanvasGetResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  // The canvas is strictly per-agent (the gateway fails closed without an
  // agent_id) — default to the first AI staff member the viewer can see.
  const effectiveAgent = agentFilter || (visibleAgents[0]?.name ?? '');

  const agentName = useCallback(
    (id: string) => agents.find((a) => a.name === id)?.display_name || id,
    [agents],
  );

  const fetchCanvas = useCallback(async () => {
    if (!effectiveAgent) return;
    try {
      const res = await api.canvas.get(effectiveAgent, viewSeq ?? undefined);
      setResult(res);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoaded(true);
    }
  }, [effectiveAgent, viewSeq]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    if (agents.length === 0) void fetchAgents();
  }, [connectionState, agents.length, fetchAgents]);

  // Initial fetch + 30s poll fallback (poll only tracks the live version;
  // a pinned history version is immutable).
  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    void fetchCanvas();
    if (viewSeq !== null) return;
    const id = setInterval(() => void fetchCanvas(), POLL_MS);
    return () => clearInterval(id);
  }, [connectionState, fetchCanvas, viewSeq]);

  // Live refresh: the gateway broadcasts `canvas.updated { agent_id, seq }`
  // whenever the agent pushes or clears.
  useEffect(() => {
    if (!effectiveAgent) return;
    return client.subscribe('canvas.updated', (payload) => {
      const data = payload as { agent_id?: string };
      if (data.agent_id === effectiveAgent && viewSeq === null) void fetchCanvas();
    });
  }, [effectiveAgent, viewSeq, fetchCanvas]);

  const canvas = result?.canvas ?? null;
  const history = result?.history ?? [];
  const currentSeq = history[0]?.seq;
  const isEmpty = !canvas || canvas.html === '';
  const viewingHistory = viewSeq !== null && viewSeq !== currentSeq;

  const versionLabel = useCallback(
    (title: string, updatedAt: string) => {
      const t = `${intl.formatDate(updatedAt, { month: 'numeric', day: 'numeric' })} ${intl.formatTime(updatedAt, { hour: '2-digit', minute: '2-digit' })}`;
      const name = title || intl.formatMessage({ id: 'canvas.untitled' });
      return `${name} · ${t}`;
    },
    [intl],
  );

  const showEmptyAgents = loaded && visibleAgents.length === 0 && scope !== 'all';

  return (
    <div className="-mx-4 -mt-4 flex min-h-0 flex-1 flex-col md:-mx-6 md:-mt-6 md:-mb-6">
      <PageHeader hideTrigger>
        <Presentation className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.canvas' })}</h1>
        <span className="hidden truncate text-sm text-muted-foreground md:block">
          {intl.formatMessage({ id: 'canvas.subtitle' })}
        </span>
        <div className="ml-auto flex items-center gap-2">
          <Select
            value={effectiveAgent}
            onValueChange={(v) => {
              setAgentFilter(String(v));
              setViewSeq(null);
              setResult(null);
              setLoaded(false);
            }}
          >
            <SelectTrigger size="sm" className="max-w-44">
              <SelectValue aria-label={intl.formatMessage({ id: 'canvas.filter.agent' })}>
                {effectiveAgent ? agentName(effectiveAgent) : ''}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              {visibleAgents.map((a) => (
                <SelectItem key={a.name} value={a.name}>
                  {a.display_name || a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {history.length > 0 && (
            <Select
              value={String(viewSeq ?? currentSeq ?? '')}
              onValueChange={(v) => {
                const seq = Number(v);
                setViewSeq(seq === currentSeq ? null : seq);
              }}
            >
              <SelectTrigger size="sm" className="max-w-48">
                <SelectValue aria-label={intl.formatMessage({ id: 'canvas.history.aria' })} />
              </SelectTrigger>
              <SelectContent>
                {history.map((v, i) => (
                  <SelectItem key={v.seq} value={String(v.seq)}>
                    {i === 0
                      ? intl.formatMessage({ id: 'canvas.version.current' })
                      : versionLabel(v.title, v.updated_at)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <Button variant="ghost" size="icon-sm" aria-label={intl.formatMessage({ id: 'canvas.actions' })} />
              }
            >
              <MoreHorizontal />
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem onClick={() => void fetchCanvas()}>
                <RefreshCw />
                {intl.formatMessage({ id: 'common.refresh' })}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </PageHeader>

      <div className="flex min-h-0 flex-1 flex-col p-4 md:p-6">
        {viewingHistory && (
          <div className="mb-3 flex items-center gap-3">
            <Badge variant="secondary" className="gap-1.5">
              <History className="size-3" aria-hidden="true" />
              {intl.formatMessage({ id: 'canvas.viewingHistory' })}
            </Badge>
            <Button variant="link" size="sm" onClick={() => setViewSeq(null)}>
              {intl.formatMessage({ id: 'canvas.backToCurrent' })}
            </Button>
          </div>
        )}

        <Card className="min-h-0 flex-1 gap-0 py-0">
          {showEmptyAgents ? (
            <div className="flex flex-1 items-center justify-center">
              <Empty icon={Presentation} title={intl.formatMessage({ id: 'canvas.noAgents' })} />
            </div>
          ) : !loaded ? (
            <div className="space-y-3 p-5">
              <Skeleton className="h-8 w-1/2" />
              <Skeleton className="h-40 w-full" />
              <Skeleton className="h-12 w-2/3" />
            </div>
          ) : error ? (
            <div className="flex flex-1 items-center justify-center">
              <Empty
                icon={Presentation}
                tone="destructive"
                title={intl.formatMessage({ id: 'canvas.error' })}
                description={error}
              />
            </div>
          ) : isEmpty ? (
            <div className="flex flex-1 items-center justify-center">
              <Empty
                icon={Presentation}
                title={intl.formatMessage({ id: 'canvas.empty' })}
                description={intl.formatMessage({ id: 'canvas.empty.hint' })}
              />
            </div>
          ) : (
            <>
              <div className="flex flex-wrap items-center gap-2 border-b border-surface-border px-5 py-3">
                <p className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">
                  {canvas.title || intl.formatMessage({ id: 'canvas.untitled' })}
                </p>
                <p className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                  {intl.formatMessage(
                    { id: 'canvas.updatedAt' },
                    {
                      time: `${intl.formatDate(canvas.updated_at, { month: 'numeric', day: 'numeric' })} ${intl.formatTime(canvas.updated_at, { hour: '2-digit', minute: '2-digit' })}`,
                    },
                  )}
                </p>
              </div>
              {/* Wide agent content scrolls INSIDE the frame (base styles wrap
                  tables/pre in overflow-x:auto); the page itself never scrolls
                  horizontally. */}
              <div className="min-h-0 min-w-0 flex-1 p-3">
                <iframe
                  title={intl.formatMessage({ id: 'canvas.frame.title' }, { agent: agentName(canvas.agent_id) })}
                  sandbox={CANVAS_SANDBOX}
                  srcDoc={canvasSrcDoc(canvas.html)}
                  className="h-full min-h-[60vh] w-full rounded-lg border border-surface-border bg-page-canvas"
                />
              </div>
              <p className="border-t border-surface-border px-5 py-3 text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'canvas.note' })}
              </p>
            </>
          )}
        </Card>
      </div>
    </div>
  );
}
