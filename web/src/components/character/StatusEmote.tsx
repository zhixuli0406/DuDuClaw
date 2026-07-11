import { cn } from '@/lib/utils';

/**
 * StatusEmote — the little head-top thought bubble a character wears to show
 * what it's doing (§3.2). The bubble body is *drawn* (rounded pill + tail); the
 * emoji is the label inside it, not a bare glyph floating in the layout. The
 * ring tint follows the `--status-agent-*` vocabulary so an emote reads as the
 * same state as the row badge / world sprite.
 */

export type StatusEmoteKind =
  | 'working'
  | 'blocked'
  | 'awaiting'
  | 'sleeping'
  | 'error'
  | 'celebrating';

const EMOTE: Record<StatusEmoteKind, { emoji: string; tint: string; label: string }> = {
  working: { emoji: '💻', tint: 'var(--status-agent-running)', label: 'Working' },
  blocked: { emoji: '⚠️', tint: 'var(--status-agent-paused)', label: 'Blocked' },
  awaiting: { emoji: '✋', tint: 'var(--status-agent-paused)', label: 'Awaiting approval' },
  sleeping: { emoji: '💤', tint: 'var(--status-agent-idle)', label: 'Resting' },
  error: { emoji: '😵', tint: 'var(--status-agent-error)', label: 'Faulted' },
  celebrating: { emoji: '🎉', tint: 'var(--status-task-icon-done)', label: 'Celebrating' },
};

export function StatusEmote({
  kind,
  size = 18,
  className,
  title,
}: {
  kind: StatusEmoteKind;
  size?: number;
  className?: string;
  title?: string;
}) {
  const e = EMOTE[kind];
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      className={cn('shrink-0', className)}
      role="img"
      aria-label={title ?? e.label}
    >
      {title ?? e.label ? <title>{title ?? e.label}</title> : null}
      {/* Drawn bubble body: rounded pill + a small tail pointing down-left. */}
      <path
        d="M6.5 20.5 L4 23 L9 20.2 Z"
        fill="var(--character-bubble)"
        stroke={e.tint}
        strokeWidth="1.1"
      />
      <rect
        x="1.4"
        y="1.4"
        width="21.2"
        height="18"
        rx="7.5"
        fill="var(--character-bubble)"
        stroke={e.tint}
        strokeWidth="1.4"
      />
      <text
        x="12"
        y="14.3"
        textAnchor="middle"
        fontSize="11"
        // Emoji glyphs ignore fill, but set it for the rare monochrome fallback.
        fill="var(--character-ink)"
      >
        {e.emoji}
      </text>
    </svg>
  );
}
