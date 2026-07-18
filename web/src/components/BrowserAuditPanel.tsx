import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useBrowserStore } from '@/stores/browser-store';
import { Globe, Eye, Monitor, Shield, Image, Clock, ChevronDown, ChevronUp } from 'lucide-react';

const tierColors: Record<string, string> = {
  L1: 'bg-success/10 text-success',
  L2: 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400',
  L3: 'bg-warning/10 text-warning',
  L4: 'bg-orange-100 text-orange-700 dark:bg-orange-900/30 dark:text-orange-400',
  L5: 'bg-destructive/10 text-destructive',
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
    <div className="rounded-xl border border-surface-border bg-surface p-5">
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Globe className="h-5 w-5 text-brand" />
          <h3 className="font-semibold text-foreground">
            {intl.formatMessage({ id: 'browser.audit.title' })}
          </h3>
        </div>
        <input
          type="text"
          value={agentFilter}
          onChange={(e) => setAgentFilter(e.target.value)}
          placeholder={intl.formatMessage({ id: 'browser.audit.filterAgent' })}
          className="w-40 rounded-lg border border-surface-border bg-muted px-3 py-1.5 text-sm text-foreground placeholder:text-muted-foreground focus:border-brand focus:outline-none focus:ring-1 focus:ring-brand"
        />
      </div>

      {auditLoading ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : auditEntries.length === 0 ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
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
                className="rounded-lg border border-surface-border bg-muted p-3 transition-colors hover:bg-surface-hover"
              >
                <div
                  className="flex cursor-pointer items-center gap-3"
                  onClick={() => setExpanded(isExpanded ? null : i)}
                >
                  {/* Tier badge */}
                  <span className={`inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-xs font-medium ${tierColors[entry.tier ?? ''] ?? 'bg-muted text-muted-foreground'}`}>
                    <TierIcon className="h-3 w-3" />
                    {entry.tier}
                  </span>

                  {/* Action */}
                  <span className="text-sm font-medium text-foreground">
                    {entry.action}
                  </span>

                  {/* Domain */}
                  {entry.domain && (
                    <span className="text-xs text-muted-foreground">
                      {entry.domain}
                    </span>
                  )}

                  {/* Agent */}
                  <span className="ml-auto text-xs text-muted-foreground">
                    {entry.agent_id}
                  </span>

                  {/* Screenshot indicator */}
                  {entry.screenshot_path && (
                    <Image className="h-3.5 w-3.5 text-brand" />
                  )}

                  {/* Timestamp */}
                  <span className="flex items-center gap-1 text-xs text-muted-foreground">
                    <Clock className="h-3 w-3" />
                    {new Date(entry.timestamp).toLocaleTimeString()}
                  </span>

                  {/* Expand toggle */}
                  {isExpanded ? (
                    <ChevronUp className="h-4 w-4 text-muted-foreground" />
                  ) : (
                    <ChevronDown className="h-4 w-4 text-muted-foreground" />
                  )}
                </div>

                {/* Expanded details */}
                {isExpanded && (
                  <div className="mt-3 space-y-2 border-t border-surface-border pt-3">
                    {entry.url && (
                      <div className="text-xs">
                        <span className="font-medium text-muted-foreground">{intl.formatMessage({ id: 'browser.audit.url' })}: </span>
                        <span className="text-foreground break-all">{entry.url}</span>
                      </div>
                    )}
                    {entry.screenshot_path && (
                      <div className="text-xs">
                        <span className="font-medium text-muted-foreground">{intl.formatMessage({ id: 'browser.audit.screenshot' })}: </span>
                        <span className="text-brand">{entry.screenshot_path}</span>
                      </div>
                    )}
                    {Object.keys(entry.details).length > 0 && (
                      <pre className="max-h-32 overflow-auto rounded-md bg-muted p-2 text-xs text-muted-foreground">
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
