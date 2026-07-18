import { useIntl } from 'react-intl';
import { Loader2, Moon } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { AgentGlyphState } from '@/stores/agent-activity-store';

/**
 * AgentStatusGlyph (WP10-T10.2) — a CSS-animated presence dot for one AI-staff
 * member. Each state has a distinct motion + colour signature so the owner can
 * read "who's busy" at a glance. All motion is disabled under
 * `prefers-reduced-motion` via the global rule in index.css. Calm Glass: amber
 * stays the scarce accent (replying / awaiting approval), everything else is a
 * muted neutral/semantic dot.
 */

type GlyphVisual = {
  /** dot colour utility (used as bg for dot states). */
  dot: string;
  /** i18n label id. */
  label: string;
};

const VISUALS: Record<AgentGlyphState, GlyphVisual> = {
  idle: { dot: 'bg-stone-400 dark:bg-stone-500', label: 'agents.glyph.idle' },
  replying: { dot: 'bg-amber-500', label: 'agents.glyph.replying' },
  tool_running: { dot: 'bg-sky-500', label: 'agents.glyph.tool_running' },
  consolidating: { dot: 'bg-violet-400', label: 'agents.glyph.consolidating' },
  awaiting_approval: { dot: 'bg-amber-500', label: 'agents.glyph.awaiting_approval' },
  paused: { dot: 'bg-amber-600/70', label: 'agents.glyph.paused' },
  terminated: { dot: 'bg-stone-400/40 dark:bg-stone-600/50', label: 'agents.glyph.terminated' },
};

export function AgentStatusGlyph({
  state,
  showLabel = false,
  className,
}: {
  state: AgentGlyphState;
  showLabel?: boolean;
  className?: string;
}) {
  const intl = useIntl();
  const visual = VISUALS[state];
  const label = intl.formatMessage({ id: visual.label });

  return (
    <span
      className={cn('inline-flex items-center gap-1.5', className)}
      title={label}
      role="status"
      aria-label={label}
    >
      <span className="relative grid h-3 w-3 place-items-center" aria-hidden="true">
        {state === 'tool_running' ? (
          <Loader2 className="h-3 w-3 animate-spin text-sky-500" />
        ) : state === 'consolidating' ? (
          <Moon className="glyph-consolidate h-3 w-3 text-violet-400" />
        ) : (
          <>
            {state === 'awaiting_approval' && (
              <span className="glyph-approval-ring absolute inset-0 rounded-full bg-amber-500/60" />
            )}
            <span
              className={cn(
                'h-2 w-2 rounded-full',
                visual.dot,
                state === 'replying' && 'glyph-replying',
              )}
            />
          </>
        )}
      </span>
      {showLabel && (
        <span className="text-[11px] font-medium text-muted-foreground">
          {label}
        </span>
      )}
    </span>
  );
}
