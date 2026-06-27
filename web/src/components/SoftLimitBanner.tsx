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

  const agentCount = agents.length;
  const channelCount = status?.channels_connected ?? 0;
  const sl = softLimitStatus(tier, agentCount, channelCount);
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
        <p className="mt-0.5 tabular-nums text-amber-700/90 dark:text-amber-300/90">
          {intl.formatMessage(
            { id: 'softLimit.usage' },
            {
              agents: agentCount,
              maxAgents: sl.limit.agents,
              channels: channelCount,
              maxChannels: sl.limit.channels,
            }
          )}
        </p>
        <p className="mt-0.5 text-amber-700/90 dark:text-amber-300/90">
          {intl.formatMessage({ id: 'softLimit.body' })}
        </p>
        <a
          href="https://duduclaw.tw/pricing"
          target="_blank"
          rel="noreferrer"
          className="mt-1.5 inline-block font-medium text-amber-700 underline underline-offset-2 hover:text-amber-900 dark:text-amber-300 dark:hover:text-amber-100"
        >
          {intl.formatMessage({ id: 'softLimit.upgrade' })}
        </a>
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
