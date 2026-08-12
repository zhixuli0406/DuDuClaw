import { Bot } from 'lucide-react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';

/**
 * AutonomyNote — the shared "what will the AI actually do" disclosure block
 * (UX audit phase4-manage-settings / phase4-daily-work, C11/C12: 12+2 Blockers
 * scattered across the dashboard wherever a control hands an AI employee a new
 * autonomous capability — installing a skill/MCP server, approving
 * `computer_use`, turning on a background planning/topology loop, letting a
 * routine run unattended — but the surrounding copy only named the feature and
 * never answered the three questions a non-technical owner actually has:
 *
 *   1. What will it start doing on its own?
 *   2. When does it stop and come back to ask me?
 *   3. How do I take that back right now?
 *
 * One small component instead of 11 bespoke call sites: `id` selects a 3-key
 * i18n bundle (`autonomy.<id>.does` / `.asks` / `.revoke`). Each call site
 * still supplies its own plain-language answers via those keys — installing a
 * skill and running an arbitrary shell command for voice transcription grant
 * very different capabilities, so a single generic sentence would not actually
 * inform anyone (the audit's own complaint about the canned ConfirmDialog copy
 * this component deliberately does NOT reuse or replace).
 *
 * Purely informational — it never gates or confirms anything itself. Existing
 * confirmation flows (ConfirmDialog, ToolApprovalPanel's approve button, …)
 * are untouched; this renders alongside them.
 */
export function AutonomyNote({ id, className }: { id: string; className?: string }) {
  const intl = useIntl();
  return (
    <div
      role="note"
      aria-label={intl.formatMessage({ id: 'autonomy.label' })}
      className={cn(
        'flex items-start gap-2.5 rounded-lg border border-brand/25 bg-brand/5 px-3 py-2.5 text-xs text-foreground',
        className,
      )}
    >
      <Bot className="mt-0.5 size-4 shrink-0 text-brand" aria-hidden="true" />
      <ul className="min-w-0 list-none space-y-1">
        <li>{intl.formatMessage({ id: `autonomy.${id}.does` })}</li>
        <li>{intl.formatMessage({ id: `autonomy.${id}.asks` })}</li>
        <li>{intl.formatMessage({ id: `autonomy.${id}.revoke` })}</li>
      </ul>
    </div>
  );
}
