import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Plug, Radio, Building2, Cpu, ChevronDown } from 'lucide-react';
import type { ComponentType } from 'react';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { filterVisible, type Gated } from '@/lib/nav-visibility';

interface Connector extends Gated {
  readonly key: string;
  readonly to: string;
  readonly icon: ComponentType<{ className?: string }>;
  readonly label: string;
}

/**
 * Connectors live behind admin in `nav-model`; mirror that here. All entries are
 * read-only shortcuts that deep-link into the dashboard (§P2.2) — configuration
 * still happens on the destination page.
 */
const CONNECTORS: Connector[] = [
  { key: 'channels', to: '/channels', icon: Radio, label: 'nav.channels', minRole: 'admin' },
  { key: 'mcp', to: '/mcp', icon: Plug, label: 'nav.mcp', minRole: 'admin' },
  { key: 'odoo', to: '/odoo', icon: Building2, label: 'nav.odoo', minRole: 'admin' },
  { key: 'inference', to: '/inference', icon: Cpu, label: 'nav.inference', minRole: 'admin' },
];

/**
 * Workspace "連接器" control (TODO-genspark-workspace-shell §P2.2). Surfaces the
 * available integrations and how many channels are live, deep-linking into the
 * dashboard. Hidden entirely when the user can see no connector (non-admins).
 */
export function ConnectorChips() {
  const intl = useIntl();
  const navigate = useNavigate();
  const status = useSystemStore((s) => s.status);
  const fetchStatus = useSystemStore((s) => s.fetchStatus);
  const role = useAuthStore((s) => s.user?.role);

  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  const isPersonal = status?.edition_profile === 'personal';
  const visible = filterVisible(CONNECTORS, role, isPersonal);

  useEffect(() => {
    if (!status) fetchStatus();
  }, [status, fetchStatus]);

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

  if (visible.length === 0) return null;

  const go = (to: string) => {
    setOpen(false);
    navigate(to);
  };

  const channelsLive = status?.channels_connected ?? 0;

  return (
    <div ref={menuRef} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        className="flex h-9 items-center gap-1.5 rounded-lg border border-surface-border px-2.5 text-xs font-medium text-muted-foreground outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <Plug className="h-4 w-4" />
        <span className="hidden sm:inline">
          {intl.formatMessage({ id: 'workspace.connectors', defaultMessage: '連接器' })}
        </span>
        {channelsLive > 0 && (
          <span className="rounded-full bg-success/15 px-1.5 text-[10px] font-semibold text-success tabular-nums">
            {channelsLive}
          </span>
        )}
        <ChevronDown className="h-3.5 w-3.5" />
      </button>

      {open && (
        <div
          role="menu"
          className="absolute bottom-full left-0 z-50 mb-2 w-56 overflow-hidden rounded-lg bg-surface-raised p-1 shadow-[var(--menu-shadow)] ring-1 ring-surface-border"
        >
          {visible.map(({ key, to, icon: Icon, label }) => (
            <button
              key={key}
              role="menuitem"
              onClick={() => go(to)}
              className="flex w-full items-center justify-between gap-2 rounded-lg px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted"
            >
              <span className="flex items-center gap-2">
                <Icon className="h-4 w-4" />
                {intl.formatMessage({ id: label })}
              </span>
              {key === 'channels' && channelsLive > 0 && (
                <span className="text-[11px] text-success tabular-nums">
                  {intl.formatMessage(
                    { id: 'workspace.channelsLive', defaultMessage: '{n} 已連線' },
                    { n: channelsLive },
                  )}
                </span>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
