import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { History, RefreshCw } from 'lucide-react';
import { Button } from '@/components/mds';
import { timeAgo } from '@/lib/format';
import { api, type ChatSessionSummary } from '@/lib/api';
import { cn } from '@/lib/utils';
import { useDismissable } from '@/hooks/useDismissable';

type LoadState = 'idle' | 'loading' | 'error' | 'ready';

/**
 * SessionHistoryMenu (WP3) — the "past conversations" entry point in the WebChat
 * header. A ghost icon button opens a popover listing the currently-selected
 * employee's history sessions (newest first); picking one hands the summary back
 * so the page can load its transcript and resume it.
 *
 * The list is fetched fresh each time the popover opens (or the target employee
 * changes while open), so it always reflects the current server state. Failures
 * surface an explicit error state with a retry — never a silent empty list.
 */
export function SessionHistoryMenu({
  agentId,
  activeSessionId,
  onResume,
}: {
  /** Agent whose sessions to list (an employee id, or the main agent id when
   *  chatting with DuDu). `null` ⇒ let the server decide (admin only). */
  agentId: string | null;
  /** The active session id — highlighted in the list. */
  activeSessionId: string | null;
  onResume: (session: ChatSessionSummary) => void;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const [state, setState] = useState<LoadState>('idle');
  const [sessions, setSessions] = useState<ChatSessionSummary[]>([]);
  const rootRef = useRef<HTMLDivElement>(null);

  const load = async () => {
    setState('loading');
    try {
      const res = await api.chatSessions.list({
        ...(agentId ? { agent_id: agentId } : {}),
        limit: 50,
      });
      setSessions(res?.sessions ?? []);
      setState('ready');
    } catch {
      setState('error');
    }
  };

  // (Re)load whenever the popover opens or the target employee changes while open.
  useEffect(() => {
    if (open) void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, agentId]);

  // Dismiss on outside click / Escape.
  useDismissable(rootRef, open, () => setOpen(false));

  const pick = (s: ChatSessionSummary) => {
    setOpen(false);
    onResume(s);
  };

  return (
    <div ref={rootRef} className="relative">
      <Button
        variant="ghost"
        size="icon"
        onClick={() => setOpen((v) => !v)}
        title={intl.formatMessage({ id: 'webchat.history', defaultMessage: '歷史對話' })}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <History />
      </Button>

      {open && (
        <div
          role="menu"
          className={cn(
            'absolute right-0 z-30 mt-2 w-80 max-w-[calc(100vw-2rem)] overflow-hidden rounded-xl',
            'border border-surface-border bg-surface shadow-[var(--menu-shadow)] backdrop-blur',
          )}
        >
          <div className="flex items-center justify-between border-b border-surface-border px-3 py-2">
            <span className="text-sm font-semibold text-foreground">
              {intl.formatMessage({ id: 'webchat.history.title', defaultMessage: '歷史對話' })}
            </span>
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => void load()}
              disabled={state === 'loading'}
              title={intl.formatMessage({ id: 'webchat.history.reload', defaultMessage: '重新整理' })}
            >
              <RefreshCw />
            </Button>
          </div>

          <div className="max-h-80 overflow-y-auto p-1.5">
            {state === 'loading' && (
              <div className="space-y-1.5 p-1">
                {[0, 1, 2].map((i) => (
                  <div
                    key={i}
                    className="h-11 animate-pulse rounded-lg bg-muted"
                  />
                ))}
              </div>
            )}

            {state === 'error' && (
              <div className="flex flex-col items-center gap-2 px-3 py-6 text-center">
                <span className="text-sm text-muted-foreground">
                  {intl.formatMessage({
                    id: 'webchat.history.error',
                    defaultMessage: '無法載入歷史對話',
                  })}
                </span>
                <Button variant="outline" size="sm" onClick={() => void load()}>
                  {intl.formatMessage({ id: 'webchat.history.retry', defaultMessage: '重試' })}
                </Button>
              </div>
            )}

            {state === 'ready' && sessions.length === 0 && (
              <div className="px-3 py-6 text-center text-sm text-muted-foreground">
                {intl.formatMessage({
                  id: 'webchat.history.empty',
                  defaultMessage: '尚無歷史對話',
                })}
              </div>
            )}

            {state === 'ready' &&
              sessions.map((s) => {
                const active = s.session_id === activeSessionId;
                return (
                  <button
                    key={s.session_id}
                    type="button"
                    role="menuitem"
                    onClick={() => pick(s)}
                    className={cn(
                      'flex w-full flex-col gap-0.5 rounded-lg px-2.5 py-2 text-left transition-colors',
                      'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
                      active
                        ? 'bg-brand/10 ring-1 ring-inset ring-brand/40'
                        : 'hover:bg-muted',
                    )}
                  >
                    <span className="truncate text-sm text-foreground">
                      {s.title.trim() ||
                        intl.formatMessage({
                          id: 'webchat.history.untitled',
                          defaultMessage: '（無標題）',
                        })}
                    </span>
                    <span className="flex items-center gap-1.5 text-xs tabular-nums text-muted-foreground">
                      <span>{timeAgo(s.last_active)}</span>
                      <span aria-hidden>·</span>
                      <span>
                        {intl.formatMessage(
                          { id: 'webchat.history.turns', defaultMessage: '{count} 輪' },
                          { count: s.turns },
                        )}
                      </span>
                    </span>
                  </button>
                );
              })}
          </div>
        </div>
      )}
    </div>
  );
}
