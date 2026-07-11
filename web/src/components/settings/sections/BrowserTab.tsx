import { useIntl } from 'react-intl';
import { ToolApprovalPanel } from '@/components/ToolApprovalPanel';
import { SessionReplayPanel } from '@/components/SessionReplayPanel';
import { BrowserAuditPanel } from '@/components/BrowserAuditPanel';
import { DecisionsPanel } from '@/components/DecisionsPanel';

// ── Browser Automation Tab ─────────────────────────────────────

export function BrowserTab() {
  const intl = useIntl();
  return (
    <div className="space-y-6">
      <p className="rounded-lg bg-stone-500/5 px-4 py-3 text-sm text-stone-500 dark:bg-white/5 dark:text-stone-400">
        {intl.formatMessage({ id: 'settings.browser.desc' })}
      </p>
      <ToolApprovalPanel />
      <SessionReplayPanel />
      <BrowserAuditPanel />
      <DecisionsPanel />
    </div>
  );
}
