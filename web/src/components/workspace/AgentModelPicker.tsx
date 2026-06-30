import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { ChevronDown, Bot, Settings2 } from 'lucide-react';
import { useChatStore } from '@/stores/chat-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useUiModeStore } from '@/stores/ui-mode-store';
import { cn } from '@/lib/utils';

/**
 * Workspace agent / model indicator (TODO-genspark-workspace-shell §P2.1),
 * the analogue of Genspark's "標準 ▾" control.
 *
 * The `/ws/chat` protocol binds the default agent server-side and exposes no
 * agent-selection field, and §0 forbids backend protocol changes. So this runs
 * in the documented **degraded mode**: it displays the live agent + model from
 * `session_info` (read-only) and offers a menu to jump to the Agents page to
 * manage / switch the active agent there.
 */
export function AgentModelPicker() {
  const intl = useIntl();
  const navigate = useNavigate();
  const setMode = useUiModeStore((s) => s.setMode);
  const agentName = useChatStore((s) => s.agentName);
  const agentIcon = useChatStore((s) => s.agentIcon);
  const model = useChatStore((s) => s.model);
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const loaded = useAgentsStore((s) => s.loaded);

  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!loaded) fetchAgents();
  }, [loaded, fetchAgents]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  const goManage = () => {
    setOpen(false);
    setMode('dashboard');
    navigate('/agents');
  };

  return (
    <div ref={menuRef} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        className="flex h-9 items-center gap-1.5 rounded-lg border border-[var(--panel-border)] px-2.5 text-xs font-medium text-stone-600 transition-colors hover:bg-stone-500/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40 dark:text-stone-300 dark:hover:bg-white/5"
      >
        <span className="text-sm leading-none">{agentIcon || '🐾'}</span>
        <span className="max-w-[8rem] truncate">{agentName || 'DuDuClaw'}</span>
        {model && <span className="hidden text-stone-400 sm:inline">· {model}</span>}
        <ChevronDown className="h-3.5 w-3.5" />
      </button>

      {open && (
        <div
          role="menu"
          className="glass-overlay absolute bottom-full left-0 z-50 mb-2 w-64 overflow-hidden rounded-xl p-1"
        >
          <p className="px-3 py-1.5 text-[11px] font-semibold uppercase tracking-wider text-stone-400">
            {intl.formatMessage({ id: 'workspace.activeAgent', defaultMessage: '目前 AI 員工' })}
          </p>
          <div className="flex items-center gap-2 rounded-lg px-3 py-2">
            <span className="text-base leading-none">{agentIcon || '🐾'}</span>
            <div className="min-w-0">
              <p className="truncate text-sm font-medium text-stone-800 dark:text-stone-100">
                {agentName || 'DuDuClaw'}
              </p>
              {model && <p className="truncate text-xs text-stone-400 tabular-nums">{model}</p>}
            </div>
          </div>

          {loaded && agents.length > 1 && (
            <p className="px-3 pt-1 text-[11px] text-stone-400">
              {intl.formatMessage(
                { id: 'workspace.agentCount', defaultMessage: '共 {count} 個 AI 員工' },
                { count: agents.length },
              )}
            </p>
          )}

          <button
            role="menuitem"
            onClick={goManage}
            className={cn(
              'mt-1 flex w-full items-center gap-2 rounded-lg px-3 py-2 text-sm text-stone-600 transition-colors',
              'hover:bg-stone-500/10 dark:text-stone-300 dark:hover:bg-white/5'
            )}
          >
            <Settings2 className="h-4 w-4" />
            {intl.formatMessage({ id: 'workspace.manageAgents', defaultMessage: '管理 AI 員工' })}
          </button>
          <p className="px-3 pb-1.5 pt-0.5 text-[11px] leading-snug text-stone-400">
            <Bot className="mr-1 inline h-3 w-3" />
            {intl.formatMessage({
              id: 'workspace.switchHint',
              defaultMessage: '切換員工請至「管理」頁設定預設員工',
            })}
          </p>
        </div>
      )}
    </div>
  );
}
