import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { AlertTriangle, PauseCircle, RadioTower, ClipboardCheck, ChevronRight } from 'lucide-react';
import { DuDu } from '@/components/mascot';
import { api, type ChannelStatus } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';

/**
 * IncidentBanner (WP14-T14.2) — the red "needs your attention" strip at the top
 * of the owner's home. Renders NOTHING when all is well (report-when-changed
 * denoising). Each incident is one glanceable chip that deep-links to the page
 * where the owner can act.
 *
 * Data sources are strictly existing read paths — no invented truth:
 *  - paused/terminated AI staff  → agents store (already loaded)
 *  - offline channels            → `channels.status`
 *  - pending approvals + open budget events → `approvalsCount` prop, supplied
 *    by DashboardPage from `approvals.list` + `budget.incidents` (WP14-T14.6/7).
 */
export function IncidentBanner({ approvalsCount = 0 }: { approvalsCount?: number }) {
  const intl = useIntl();
  const agents = useAgentsStore((s) => s.agents);
  const connectionState = useConnectionStore((s) => s.state);
  const [channels, setChannels] = useState<ReadonlyArray<ChannelStatus>>([]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    api.channels
      .status()
      .then((res) => setChannels(res?.channels ?? []))
      .catch(() => setChannels([]));
  }, [connectionState]);

  const suspended = agents.filter((a) => a.status === 'paused' || a.status === 'terminated').length;
  const offline = channels.filter((c) => !c.connected).length;

  const incidents: Array<{ key: string; icon: typeof AlertTriangle; label: string; to: string }> = [];
  if (suspended > 0) {
    incidents.push({
      key: 'suspended',
      icon: PauseCircle,
      label: intl.formatMessage({ id: 'dashboard.incident.suspended' }, { count: suspended }),
      to: '/agents',
    });
  }
  if (offline > 0) {
    incidents.push({
      key: 'channels',
      icon: RadioTower,
      label: intl.formatMessage({ id: 'dashboard.incident.channels' }, { count: offline }),
      to: '/channels',
    });
  }
  if (approvalsCount > 0) {
    incidents.push({
      key: 'approvals',
      icon: ClipboardCheck,
      label: intl.formatMessage({ id: 'dashboard.incident.approvals' }, { count: approvalsCount }),
      to: '/approvals',
    });
  }

  // Report-when-changed: fully silent when nothing needs attention.
  if (incidents.length === 0) return null;

  return (
    <div className="rounded-xl border border-rose-500/30 bg-rose-500/8 p-3 dark:border-rose-400/25 dark:bg-rose-400/10">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <DuDu face="concerned" size={28} className="shrink-0" />
        <span className="inline-flex items-center gap-1.5 text-sm font-semibold text-rose-700 dark:text-rose-300">
          <AlertTriangle className="h-4 w-4" />
          {intl.formatMessage({ id: 'dashboard.incident.title' })}
        </span>
        <div className="flex flex-wrap items-center gap-2">
          {incidents.map(({ key, icon: Icon, label, to }) => (
            <Link
              key={key}
              to={to}
              className="group inline-flex items-center gap-1.5 rounded-full bg-white/70 px-3 py-1 text-xs font-medium text-rose-700 ring-1 ring-inset ring-rose-500/25 transition-colors hover:bg-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-rose-500/50 dark:bg-stone-900/40 dark:text-rose-200 dark:hover:bg-stone-900/70"
            >
              <Icon className="h-3.5 w-3.5" />
              {label}
              <ChevronRight className="h-3 w-3 opacity-60 transition-transform group-hover:translate-x-0.5" />
            </Link>
          ))}
        </div>
      </div>
    </div>
  );
}
