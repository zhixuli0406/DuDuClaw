import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { History, Presentation } from 'lucide-react';
import { api, type CanvasGetResult } from '@/lib/api';
import { canvasSrcDoc, CANVAS_SANDBOX } from '@/lib/canvas-doc';
import { client } from '@/lib/ws-client';
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
  controlClass,
} from '@/components/ui';

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
    <Page wide>
      <PageHeader
        icon={Presentation}
        title={intl.formatMessage({ id: 'nav.canvas' })}
        subtitle={intl.formatMessage({ id: 'canvas.subtitle' })}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <select
              aria-label={intl.formatMessage({ id: 'canvas.filter.agent' })}
              className={`${controlClass} max-w-56`}
              value={effectiveAgent}
              onChange={(e) => {
                setAgentFilter(e.target.value);
                setViewSeq(null);
                setResult(null);
                setLoaded(false);
              }}
            >
              {visibleAgents.map((a) => (
                <option key={a.name} value={a.name}>
                  {a.display_name || a.name}
                </option>
              ))}
            </select>
            {history.length > 0 && (
              <select
                aria-label={intl.formatMessage({ id: 'canvas.history.aria' })}
                className={`${controlClass} max-w-64`}
                value={viewSeq ?? currentSeq ?? ''}
                onChange={(e) => {
                  const seq = Number(e.target.value);
                  setViewSeq(seq === currentSeq ? null : seq);
                }}
              >
                {history.map((v, i) => (
                  <option key={v.seq} value={v.seq}>
                    {i === 0
                      ? intl.formatMessage({ id: 'canvas.version.current' })
                      : versionLabel(v.title, v.updated_at)}
                  </option>
                ))}
              </select>
            )}
          </div>
        }
      />

      {viewingHistory && (
        <div className="mb-3 flex items-center gap-3">
          <Badge tone="warning" dot>
            <History className="mr-1 inline h-3 w-3" aria-hidden="true" />
            {intl.formatMessage({ id: 'canvas.viewingHistory' })}
          </Badge>
          <button
            type="button"
            className="text-sm font-medium text-amber-600 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 dark:text-amber-400"
            onClick={() => setViewSeq(null)}
          >
            {intl.formatMessage({ id: 'canvas.backToCurrent' })}
          </button>
        </div>
      )}

      <Card padded={false} bodyClassName="flex min-h-[420px] flex-col">
        {showEmptyAgents ? (
          <div className="flex flex-1 items-center justify-center p-6">
            <EmptyState
              dudu={{ face: 'sleep' }}
              title={intl.formatMessage({ id: 'canvas.noAgents' })}
            />
          </div>
        ) : !loaded ? (
          <div className="space-y-3 p-5">
            <Skeleton className="h-8 w-1/2" />
            <Skeleton className="h-40 w-full" />
            <Skeleton className="h-12 w-2/3" />
          </div>
        ) : error ? (
          <div className="flex flex-1 items-center justify-center p-6">
            <EmptyState
              dudu={{ face: 'concerned' }}
              title={intl.formatMessage({ id: 'canvas.error' })}
              hint={error}
            />
          </div>
        ) : isEmpty ? (
          <div className="flex flex-1 items-center justify-center p-6">
            <EmptyState
              dudu={{ face: 'curious' }}
              title={intl.formatMessage({ id: 'canvas.empty' })}
              hint={intl.formatMessage({ id: 'canvas.empty.hint' })}
            />
          </div>
        ) : (
          <>
            <div className="flex flex-wrap items-center gap-2 border-b border-[var(--panel-border)] px-5 py-3">
              <p className="min-w-0 flex-1 truncate text-sm font-semibold text-stone-800 dark:text-stone-100">
                {canvas.title || intl.formatMessage({ id: 'canvas.untitled' })}
              </p>
              <p className="shrink-0 text-xs text-stone-400 tabular-nums dark:text-stone-500">
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
            <div className="min-w-0 flex-1 p-3">
              <iframe
                title={intl.formatMessage(
                  { id: 'canvas.frame.title' },
                  { agent: agentName(canvas.agent_id) },
                )}
                sandbox={CANVAS_SANDBOX}
                srcDoc={canvasSrcDoc(canvas.html)}
                className="h-[68vh] w-full rounded-xl border border-[var(--panel-border)] bg-stone-50 dark:bg-stone-900"
              />
            </div>
            <p className="border-t border-[var(--panel-border)] px-5 py-3 text-xs text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'canvas.note' })}
            </p>
          </>
        )}
      </Card>
    </Page>
  );
}
