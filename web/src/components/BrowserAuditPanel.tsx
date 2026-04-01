import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useBrowserStore } from '@/stores/browser-store';
import { Globe, Eye, Monitor, Shield, Image, Clock, ChevronDown, ChevronUp } from 'lucide-react';

const tierColors: Record<string, string> = {
  L1: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
  L2: 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400',
  L3: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  L4: 'bg-orange-100 text-orange-700 dark:bg-orange-900/30 dark:text-orange-400',
  L5: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
};

const tierIcons: Record<string, typeof Globe> = {
  L1: Globe,
  L2: Eye,
  L3: Monitor,
  L4: Shield,
  L5: Monitor,
};

export function BrowserAuditPanel() {
  const intl = useIntl();
  const { auditEntries, auditLoading, fetchAuditLog } = useBrowserStore();
  const [agentFilter, setAgentFilter] = useState('');
  const [expanded, setExpanded] = useState<number | null>(null);

  useEffect(() => {
    fetchAuditLog(30, agentFilter || undefined);
  }, [agentFilter, fetchAuditLog]);

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-700 dark:bg-stone-800/50">
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Globe className="h-5 w-5 text-amber-500" />
          <h3 className="font-semibold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'browser.audit.title' })}
          </h3>
        </div>
        <input
          type="text"
          value={agentFilter}
          onChange={(e) => setAgentFilter(e.target.value)}
          placeholder={intl.formatMessage({ id: 'browser.audit.filterAgent' })}
          className="w-40 rounded-lg border border-stone-300 bg-stone-50 px-3 py-1.5 text-sm text-stone-700 placeholder:text-stone-400 focus:border-amber-400 focus:outline-none focus:ring-1 focus:ring-amber-400 dark:border-stone-600 dark:bg-stone-700 dark:text-stone-200 dark:placeholder:text-stone-500"
        />
      </div>

      {auditLoading ? (
        <p className="py-8 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : auditEntries.length === 0 ? (
        <p className="py-8 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'browser.audit.empty' })}
        </p>
      ) : (
        <div className="max-h-96 space-y-2 overflow-y-auto">
          {auditEntries.map((entry, i) => {
            const TierIcon = tierIcons[entry.tier ?? ''] ?? Globe;
            const isExpanded = expanded === i;
            return (
              <div
                key={`${entry.timestamp}-${i}`}
                className="rounded-lg border border-stone-100 bg-stone-50/50 p-3 transition-colors hover:bg-stone-100/50 dark:border-stone-700 dark:bg-stone-800 dark:hover:bg-stone-750"
              >
                <div
                  className="flex cursor-pointer items-center gap-3"
                  onClick={() => setExpanded(isExpanded ? null : i)}
                >
                  {/* Tier badge */}
                  <span className={`inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-xs font-medium ${tierColors[entry.tier ?? ''] ?? 'bg-stone-100 text-stone-600'}`}>
                    <TierIcon className="h-3 w-3" />
                    {entry.tier}
                  </span>

                  {/* Action */}
                  <span className="text-sm font-medium text-stone-700 dark:text-stone-200">
                    {entry.action}
                  </span>

                  {/* Domain */}
                  {entry.domain && (
                    <span className="text-xs text-stone-500 dark:text-stone-400">
                      {entry.domain}
                    </span>
                  )}

                  {/* Agent */}
                  <span className="ml-auto text-xs text-stone-400">
                    {entry.agent_id}
                  </span>

                  {/* Screenshot indicator */}
                  {entry.screenshot_path && (
                    <Image className="h-3.5 w-3.5 text-amber-500" />
                  )}

                  {/* Timestamp */}
                  <span className="flex items-center gap-1 text-xs text-stone-400">
                    <Clock className="h-3 w-3" />
                    {new Date(entry.timestamp).toLocaleTimeString()}
                  </span>

                  {/* Expand toggle */}
                  {isExpanded ? (
                    <ChevronUp className="h-4 w-4 text-stone-400" />
                  ) : (
                    <ChevronDown className="h-4 w-4 text-stone-400" />
                  )}
                </div>

                {/* Expanded details */}
                {isExpanded && (
                  <div className="mt-3 space-y-2 border-t border-stone-200 pt-3 dark:border-stone-700">
                    {entry.url && (
                      <div className="text-xs">
                        <span className="font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'browser.audit.url' })}: </span>
                        <span className="text-stone-700 dark:text-stone-300 break-all">{entry.url}</span>
                      </div>
                    )}
                    {entry.screenshot_path && (
                      <div className="text-xs">
                        <span className="font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'browser.audit.screenshot' })}: </span>
                        <span className="text-amber-600 dark:text-amber-400">{entry.screenshot_path}</span>
                      </div>
                    )}
                    {Object.keys(entry.details).length > 0 && (
                      <pre className="max-h-32 overflow-auto rounded-md bg-stone-100 p-2 text-xs text-stone-600 dark:bg-stone-900 dark:text-stone-400">
                        {JSON.stringify(entry.details, null, 2)}
                      </pre>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
