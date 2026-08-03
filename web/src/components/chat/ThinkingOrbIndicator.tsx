/**
 * ThinkingOrbIndicator — DuDuClaw's chat "thinking" / tool-execution status
 * glyph, built on top of the `thinking-orbs` npm package.
 *
 * Source & license: https://github.com/Jakubantalik/thinking-orbs
 * Copyright (c) Jakub Antalik — MIT License (see web/node_modules/thinking-orbs/LICENSE).
 * `thinking-orbs` ships nine hand-tuned Canvas-2D loading animations designed
 * for AI/agent UIs; we only surface the six that map onto DuDuClaw's own
 * chat/tool-execution states (see STATE_TO_ORB below).
 *
 * Reduced motion: `thinking-orbs` already gates its internal
 * requestAnimationFrame loop behind a live `matchMedia('(prefers-reduced-motion: reduce)')`
 * listener (never starts the loop, paints one static frame instead) — but we
 * additionally gate at this wrapper level with our own listener so DuDuClaw's
 * reduced-motion contract is self-contained and testable without relying on
 * a third-party package's internals: when reduced motion is preferred we
 * never mount the Canvas-driving <ThinkingOrb> at all and render a plain
 * static glyph instead.
 */
import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { ThinkingOrb, type OrbSize, type OrbState } from 'thinking-orbs';
import { cn } from '@/lib/utils';

/**
 * DuDuClaw's own chat/tool-execution state vocabulary (see
 * `wiki/duduclaw-kb/thinking-orbs-animations` mapping table). Kept distinct
 * from `thinking-orbs`' `OrbState` so call sites reason about DuDuClaw
 * concepts, not the library's animation names.
 */
export type ThinkingState =
  /** Chat waiting for the AI's reply, empty/center-stage context. */
  | 'waiting'
  /** Chat waiting for the AI's reply, inline in the message stream. */
  | 'inline'
  /** Memory / wiki search, doctor probes. */
  | 'searching'
  /** Goal-loop judge verdict in progress / task in_review. */
  | 'solving'
  /** Voice (ASR) actively listening. */
  | 'listening'
  /** Skill synthesis / provisioning. */
  | 'shaping';

const STATE_TO_ORB: Record<ThinkingState, { orb: OrbState; size: OrbSize }> = {
  waiting: { orb: 'working', size: 64 },
  inline: { orb: 'composing', size: 20 },
  searching: { orb: 'searching', size: 20 },
  solving: { orb: 'solving', size: 20 },
  listening: { orb: 'listening', size: 20 },
  shaping: { orb: 'shaping', size: 20 },
};

const DEFAULT_LABELS: Record<ThinkingState, string> = {
  waiting: 'Thinking…',
  inline: 'Composing a reply…',
  searching: 'Searching…',
  solving: 'Reviewing…',
  listening: 'Listening…',
  shaping: 'Synthesizing a skill…',
};

/** Live-updating `prefers-reduced-motion` gate, independent of the library's own. */
function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() =>
    typeof window !== 'undefined' && typeof window.matchMedia === 'function'
      ? window.matchMedia('(prefers-reduced-motion: reduce)').matches
      : false,
  );

  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
    const onChange = () => setReduced(mq.matches);
    onChange();
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  return reduced;
}

/** Static, motion-free replacement painted when reduced motion is preferred. */
function StaticThinkingGlyph({
  size,
  label,
  className,
  decorative,
}: {
  size: OrbSize;
  label: string;
  className?: string;
  decorative: boolean;
}) {
  return (
    <span
      role={decorative ? undefined : 'img'}
      aria-label={decorative ? undefined : label}
      aria-hidden={decorative ? true : undefined}
      data-thinking-state="static"
      className={cn(
        'inline-flex shrink-0 items-center justify-center rounded-full border border-dashed border-muted-foreground/40',
        className,
      )}
      style={{ width: size, height: size }}
    >
      <span
        className="block rounded-full bg-muted-foreground/60"
        style={{ width: Math.max(4, size * 0.28), height: Math.max(4, size * 0.28) }}
      />
    </span>
  );
}

export interface ThinkingOrbIndicatorProps {
  /** Which DuDuClaw chat/tool state to show. */
  state: ThinkingState;
  /** Override the preset size (defaults to the size tuned for `state`). */
  size?: OrbSize;
  /** Override the accessible label (defaults to a localized per-state label). */
  label?: string;
  className?: string;
  /**
   * Mark the orb as purely decorative (`aria-hidden`, no `aria-label`/`role="img"`)
   * for contexts where an ancestor already announces the same state as text
   * (e.g. a `role="status"` pill) — avoids screen readers double-announcing
   * "Listening… Listening…". Defaults to `false` (the orb is the only
   * accessible description of the state, e.g. inline in a message stream).
   */
  decorative?: boolean;
}

/**
 * Renders the tuned Canvas animation for a DuDuClaw thinking/tool-execution
 * state, or a static motion-free glyph when the user prefers reduced motion.
 */
export function ThinkingOrbIndicator({
  state,
  size,
  label,
  className,
  decorative = false,
}: ThinkingOrbIndicatorProps) {
  const intl = useIntl();
  const reducedMotion = usePrefersReducedMotion();
  const preset = STATE_TO_ORB[state];
  const resolvedSize = size ?? preset.size;
  const resolvedLabel =
    label ??
    intl.formatMessage({ id: `chat.thinking.${state}`, defaultMessage: DEFAULT_LABELS[state] });

  if (reducedMotion) {
    return (
      <StaticThinkingGlyph
        size={resolvedSize}
        label={resolvedLabel}
        className={className}
        decorative={decorative}
      />
    );
  }

  return (
    <ThinkingOrb
      state={preset.orb}
      size={resolvedSize}
      aria-label={decorative ? undefined : resolvedLabel}
      aria-hidden={decorative ? true : undefined}
      className={className}
    />
  );
}
