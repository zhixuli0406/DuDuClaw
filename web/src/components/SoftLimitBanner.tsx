import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { TrendingUp, X } from 'lucide-react';
import { api } from '@/lib/api';
import { useSystemStore } from '@/stores/system-store';
import { useAgentsStore } from '@/stores/agents-store';
import { softLimitStatus } from '@/lib/soft-limits';

/**
 * Non-blocking banner shown when a personal cloud tenant reaches its plan's
 * soft agent/channel limit. Informational only — it never prevents any action
 * (matches the existing Hobby soft-limit behavior). Renders nothing for
 * unlimited tiers or when under the limit.
 */
export function SoftLimitBanner() {
  const intl = useIntl();
  const status = useSystemStore((s) => s.status);
  const agents = useAgentsStore((s) => s.agents);
  const [tier, setTier] = useState<string | undefined>();
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    let active = true;
    api.system
      .version()
      .then((v) => {
        if (active) setTier(v.edition);
      })
      .catch(() => {
        /* version is best-effort; no banner if it fails */
      });
    return () => {
      active = false;
    };
  }, []);

  const sl = softLimitStatus(tier, agents.length, status?.channels_connected ?? 0);
  if (!sl || !sl.anyOver || dismissed) return null;

  return (
    <div className="mb-4 flex items-start gap-3 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-200">
      <TrendingUp className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="flex-1">
        <p className="font-medium">
          {intl.formatMessage(
            { id: 'softLimit.title' },
            { tier: sl.tier }
          )}
        </p>
        <p className="mt-0.5 text-amber-700/90 dark:text-amber-300/90">
          {intl.formatMessage({ id: 'softLimit.body' })}
        </p>
      </div>
      <button
        onClick={() => setDismissed(true)}
        className="rounded p-1 text-amber-700/70 transition-colors hover:bg-amber-500/15 dark:text-amber-300/70"
        aria-label={intl.formatMessage({ id: 'softLimit.dismiss' })}
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}
