import type { ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';

/**
 * LiveBadge — a pulsing dot marking an in-flight run (paperclip P2/P5 "Live").
 * The ping ring is CSS-only and is stilled by the global reduced-motion rule.
 */
export function LiveBadge({
  children,
  label,
  className,
}: {
  /** Optional trailing text (e.g. a run count). Defaults to a localized "Live". */
  children?: ReactNode;
  /** Accessible label when there is no visible text. */
  label?: string;
  className?: string;
}) {
  const intl = useIntl();
  const text =
    children ?? intl.formatMessage({ id: 'live.badge', defaultMessage: 'Live' });
  const aria = label ?? (typeof text === 'string' ? text : 'Live');
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium',
        'text-[color:var(--status-agent-running)] ring-1 ring-inset ring-[color:var(--status-agent-running)]/30',
        className,
      )}
      aria-label={aria}
    >
      <span className="relative inline-flex h-1.5 w-1.5 shrink-0" aria-hidden="true">
        <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-[color:var(--status-agent-running)] opacity-60" />
        <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-[color:var(--status-agent-running)]" />
      </span>
      {text}
    </span>
  );
}
