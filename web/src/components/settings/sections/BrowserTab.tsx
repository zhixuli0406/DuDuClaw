import { ToolApprovalPanel } from '@/components/ToolApprovalPanel';
import { SessionReplayPanel } from '@/components/SessionReplayPanel';
import { BrowserAuditPanel } from '@/components/BrowserAuditPanel';
import { DecisionsPanel } from '@/components/DecisionsPanel';

// ── Browser Automation Tab ─────────────────────────────────────

export function BrowserTab() {
  return (
    <div className="space-y-6">
      <ToolApprovalPanel />
      <SessionReplayPanel />
      <BrowserAuditPanel />
      <DecisionsPanel />
    </div>
  );
}
