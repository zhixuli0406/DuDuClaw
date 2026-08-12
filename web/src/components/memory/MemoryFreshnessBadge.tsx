import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { Badge, Tooltip, TooltipTrigger, TooltipContent } from '@/components/mds';
import {
  FRESHNESS_BANDS,
  FRESHNESS_STYLES,
  freshnessBand,
  freshnessHintId,
  freshnessLabelId,
  type FreshnessId,
} from '@/lib/memory-freshness';
import {
  SparklesIcon,
  ShieldCheckIcon,
  HourglassIcon,
  ArchiveIcon,
} from 'lucide-react';

/**
 * The freshness badge and its legend.
 *
 * A memory is not a permanent record — the platform lets rarely-used ones fade
 * and eventually files them away. That was previously invisible: the page
 * showed every memory identically right up until one silently disappeared.
 * These two components put the state on the row and explain, in one sentence,
 * the one thing a user can do about it (recall it, and it lasts longer).
 *
 * Deliberately no numbers on the surface: the underlying 0–1 figure and the
 * formula behind it are implementation detail. Four states, four icons, four
 * sentences.
 */

/** One icon per state — the shape, not just the colour, tells them apart. */
const FRESHNESS_ICONS: Record<FreshnessId, typeof SparklesIcon> = {
  fresh: SparklesIcon,
  stable: ShieldCheckIcon,
  fading: HourglassIcon,
  archiving: ArchiveIcon,
};

/**
 * Freshness state for one memory. Renders nothing when the backend sent no
 * decay figure (an older gateway) rather than guessing a state.
 */
export function MemoryFreshnessBadge({
  retrievability,
  className,
}: {
  retrievability: number | null | undefined;
  className?: string;
}) {
  const intl = useIntl();
  const band = freshnessBand(retrievability);
  if (!band) return null;

  const style = FRESHNESS_STYLES[band];
  const Icon = FRESHNESS_ICONS[band];
  const label = intl.formatMessage({ id: freshnessLabelId(band) });
  const hint = intl.formatMessage({ id: freshnessHintId(band) });

  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Badge
            variant="outline"
            // Focusable so the explanation is reachable by keyboard, not just
            // on hover; `aria-label` carries it for screen readers regardless.
            tabIndex={0}
            aria-label={`${label} — ${hint}`}
            className={cn('gap-1', style.badge, className)}
          >
            <Icon className={style.tone} aria-hidden />
            {label}
          </Badge>
        }
      />
      <TooltipContent className="max-w-64">{hint}</TooltipContent>
    </Tooltip>
  );
}

/**
 * Legend above the list. Without it the badges are four unexplained words; with
 * it the list header answers "what are these states, and in what order?" once,
 * instead of every row carrying its own explanation.
 */
export function MemoryFreshnessLegend({ className }: { className?: string }) {
  const intl = useIntl();
  return (
    <ul
      aria-label={intl.formatMessage({ id: 'memory.freshness.legend' })}
      className={cn('flex flex-wrap items-center gap-x-3 gap-y-1', className)}
    >
      {FRESHNESS_BANDS.map(({ id }) => {
        const style = FRESHNESS_STYLES[id];
        const Icon = FRESHNESS_ICONS[id];
        return (
          <li key={id} className="flex items-center gap-1 text-xs text-muted-foreground">
            <Icon className={cn('size-3.5 shrink-0', style.tone)} aria-hidden />
            {intl.formatMessage({ id: freshnessLabelId(id) })}
          </li>
        );
      })}
    </ul>
  );
}
