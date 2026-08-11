import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { CheckCircle2, Users, XCircle } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import { api, type RuntimeDetect, type TakeoverRecord } from '@/lib/api';
import { Empty, Skeleton } from '@/components/mds';
import { useDevPanelIdJump } from './id-jump';

const TAKEOVER_POLL_MS = 15000;

/** `runtime.detect` boolean fields, in the display order the panel renders
 *  them. Provider names are proper nouns (kept as-is across locales, same
 *  convention as `agents.runtime.provider.*` for the four that overlap it —
 *  `antigravity` has no entry there yet, so it's a literal here too). */
const RUNTIME_PROVIDERS: ReadonlyArray<{
  readonly key: keyof Pick<RuntimeDetect, 'claude_cli' | 'codex' | 'gemini' | 'antigravity' | 'grok'>;
  readonly label: string;
}> = [
  { key: 'claude_cli', label: 'Claude' },
  { key: 'codex', label: 'Codex' },
  { key: 'gemini', label: 'Gemini' },
  { key: 'antigravity', label: 'Antigravity' },
  { key: 'grok', label: 'Grok' },
];

function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

/**
 * 系統 tab — `system.status` (already cached in `useSystemStore`, fetched
 * shell-wide by `MainLayout`) + `runtime.detect` + the W3-1 `takeover.list`
 * read-only surface, so a manager can see who — if anyone — currently holds
 * a live conversation without leaving whatever page they're on.
 */
export function DevPanelSystemTab() {
  const connectionState = useConnectionStore((s) => s.state);
  const status = useSystemStore((s) => s.status);

  return (
    <div className="flex flex-col gap-4">
      <SystemStatusSection status={status} />
      <RuntimeSection connectionState={connectionState} />
      <TakeoverSection connectionState={connectionState} />
    </div>
  );
}

function StatTile({ label, value }: { readonly label: string; readonly value: string }) {
  return (
    <div className="rounded-lg border border-surface-border bg-surface px-2.5 py-1.5">
      <div className="text-[10px] text-muted-foreground">{label}</div>
      <div className="font-mono text-sm tabular-nums text-foreground">{value}</div>
    </div>
  );
}

function SystemStatusSection({
  status,
}: {
  readonly status: { version: string; uptime_seconds: number; agents_count: number; channels_connected: number } | null;
}) {
  const intl = useIntl();
  return (
    <section>
      <h3 className="mb-1.5 text-xs font-semibold text-foreground">
        {intl.formatMessage({ id: 'devpanel.system.statusTitle' })}
      </h3>
      {status ? (
        <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-4">
          <StatTile label={intl.formatMessage({ id: 'devpanel.system.version' })} value={status.version} />
          <StatTile
            label={intl.formatMessage({ id: 'devpanel.system.uptime' })}
            value={formatUptime(status.uptime_seconds)}
          />
          <StatTile
            label={intl.formatMessage({ id: 'devpanel.system.agentsCount' })}
            value={String(status.agents_count)}
          />
          <StatTile
            label={intl.formatMessage({ id: 'devpanel.system.channelsConnected' })}
            value={String(status.channels_connected)}
          />
        </div>
      ) : (
        <Skeleton className="h-14 w-full rounded-lg" />
      )}
    </section>
  );
}

function RuntimeSection({ connectionState }: { readonly connectionState: string }) {
  const intl = useIntl();
  const [detect, setDetect] = useState<RuntimeDetect | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;
    api.runtime
      .detect()
      .then((res) => {
        if (!cancelled) setDetect(res);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [connectionState]);

  return (
    <section>
      <h3 className="mb-1.5 text-xs font-semibold text-foreground">
        {intl.formatMessage({ id: 'devpanel.system.runtimeTitle' })}
      </h3>
      {failed ? (
        <p className="py-2 text-center text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'devpanel.system.loadError' })}
        </p>
      ) : !detect ? (
        <Skeleton className="h-8 w-full rounded-lg" />
      ) : (
        <div className="flex flex-wrap items-center gap-1.5">
          {RUNTIME_PROVIDERS.map(({ key, label }) => {
            const installed = Boolean(detect[key]);
            return (
              <span
                key={key}
                className={cn(
                  'inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium',
                  installed
                    ? 'border-success/30 bg-success/10 text-success'
                    : 'border-surface-border text-muted-foreground',
                )}
                title={intl.formatMessage({
                  id: installed ? 'devpanel.system.runtime.installed' : 'devpanel.system.runtime.notInstalled',
                })}
              >
                {installed ? (
                  <CheckCircle2 className="size-3" aria-hidden="true" />
                ) : (
                  <XCircle className="size-3" aria-hidden="true" />
                )}
                {label}
              </span>
            );
          })}
          <span
            className={cn(
              'inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium',
              detect.claude_oauth
                ? 'border-success/30 bg-success/10 text-success'
                : 'border-surface-border text-muted-foreground',
            )}
          >
            {detect.claude_oauth ? (
              <CheckCircle2 className="size-3" aria-hidden="true" />
            ) : (
              <XCircle className="size-3" aria-hidden="true" />
            )}
            OAuth
            {detect.claude_subscription ? ` · ${detect.claude_subscription}` : ''}
          </span>
        </div>
      )}
    </section>
  );
}

function TakeoverSection({ connectionState }: { readonly connectionState: string }) {
  const intl = useIntl();
  const { jumpToAgent, jumpToConversation } = useDevPanelIdJump();
  const [items, setItems] = useState<TakeoverRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;

    const load = (isFirst: boolean) => {
      api.takeover
        .list()
        .then((res) => {
          if (cancelled) return;
          setItems(res?.items ?? []);
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
    const id = setInterval(() => load(false), TAKEOVER_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [connectionState]);

  return (
    <section>
      <h3 className="mb-1.5 text-xs font-semibold text-foreground">
        {intl.formatMessage({ id: 'devpanel.system.takeoverTitle' })}
      </h3>
      {loading ? (
        <Skeleton className="h-16 w-full rounded-lg" />
      ) : failed ? (
        <p className="py-2 text-center text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'devpanel.system.loadError' })}
        </p>
      ) : items.length === 0 ? (
        <Empty
          icon={Users}
          title={intl.formatMessage({ id: 'devpanel.system.takeoverEmpty' })}
          className="py-6"
        />
      ) : (
        <div className="space-y-1">
          {items.map((rec) => (
            <div
              key={rec.conversation}
              className="rounded-lg border border-surface-border bg-surface px-2.5 py-1.5 text-xs"
            >
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                  {rec.channel_label}
                </span>
                <button
                  type="button"
                  onClick={() => jumpToAgent(rec.agent_id)}
                  className="font-mono text-[11px] font-medium text-brand hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                >
                  {rec.agent_id}
                </button>
                <span className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground">
                  {intl.formatMessage({ id: 'devpanel.system.minutesLeft.value' }, { minutes: rec.minutes_left })}
                </span>
              </div>
              <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-muted-foreground">
                <span>
                  {intl.formatMessage({ id: 'devpanel.system.col.holder' })}: {rec.holder_display}
                </span>
                <button
                  type="button"
                  onClick={() => void jumpToConversation(rec.conversation)}
                  className="truncate font-mono hover:text-brand hover:underline"
                  title={rec.conversation}
                >
                  {intl.formatMessage({ id: 'devpanel.system.col.conversation' })}: {rec.conversation}
                </button>
                {rec.claimed_task_ids.length > 0 && (
                  <span>
                    {intl.formatMessage(
                      { id: 'devpanel.system.claimedTasks' },
                      { count: rec.claimed_task_ids.length },
                    )}
                  </span>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
