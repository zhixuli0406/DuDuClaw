import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { ChevronDown, Check } from 'lucide-react';
import { useAgentsStore } from '@/stores/agents-store';
import { useEffectiveName, useEffectiveLogo } from '@/lib/branding';
import { cn } from '@/lib/utils';

/**
 * "選 AI 員工" control for the 交辦 panel (UX plan I-1b).
 *
 * Was a read-only mirror of the `/ws/chat` session's bound agent (that protocol
 * exposes no selection field) for a workspace landing that never shipped. The
 * assign panel genuinely picks a target — `tasks.goal_create` takes `agent_id`
 * — so it is now a **controlled picker** over the live roster. Still no
 * hardcoded model list (that would violate the multi-runtime principle): the
 * per-agent model shown under each name is whatever `agent.model.preferred`
 * already says.
 */
export function AgentModelPicker({
  value,
  onChange,
}: {
  value: string;
  onChange: (agentId: string) => void;
}) {
  const intl = useIntl();
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const loaded = useAgentsStore((s) => s.loaded);
  const brandName = useEffectiveName();
  const brandLogo = useEffectiveLogo();
  const fallbackIcon = brandLogo.isImage ? '🐾' : brandLogo.value;

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

  const selected = agents.find((a) => a.name === value) ?? null;
  const triggerLabel = selected
    ? selected.display_name || selected.name
    : intl.formatMessage({ id: 'assign.agentPlaceholder' });

  return (
    <div ref={menuRef} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        // Name the control AND the current pick — a bare "選 AI 員工" label
        // would hide who is actually selected from assistive tech.
        aria-label={`${intl.formatMessage({ id: 'assign.agent' })}: ${triggerLabel}`}
        className="flex h-9 items-center gap-1.5 rounded-lg border border-surface-border px-2.5 text-xs font-medium text-muted-foreground outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <span className="text-sm leading-none">{selected?.icon || fallbackIcon}</span>
        <span className="max-w-[8rem] truncate">{triggerLabel || brandName}</span>
        <ChevronDown className="h-3.5 w-3.5" />
      </button>

      {open && (
        <div
          role="menu"
          className="absolute bottom-full left-0 z-50 mb-2 max-h-72 w-64 overflow-y-auto rounded-lg bg-surface-raised p-1 shadow-[var(--menu-shadow)] ring-1 ring-surface-border"
        >
          <p className="px-3 py-1.5 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            {intl.formatMessage({ id: 'assign.agent' })}
          </p>
          {agents.length === 0 && (
            <p className="px-3 py-2 text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'tasks.assignee.empty' })}
            </p>
          )}
          {agents.map((a) => {
            const active = a.name === value;
            return (
              <button
                key={a.name}
                role="menuitemradio"
                aria-checked={active}
                onClick={() => {
                  onChange(a.name);
                  setOpen(false);
                }}
                className={cn(
                  'flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left text-sm transition-colors hover:bg-muted',
                  active ? 'text-foreground' : 'text-muted-foreground'
                )}
              >
                <span className="text-base leading-none">{a.icon || fallbackIcon}</span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-medium text-foreground">
                    {a.display_name || a.name}
                  </span>
                  {a.model?.preferred && (
                    <span className="block truncate text-xs text-muted-foreground tabular-nums">
                      {a.model.preferred}
                    </span>
                  )}
                </span>
                {active && <Check className="h-4 w-4 shrink-0 text-brand" />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
