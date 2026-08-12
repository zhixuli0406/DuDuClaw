/**
 * ThinkingOrbIndicator — DuDuClaw's chat "thinking" / tool-execution status
 * glyph. Maps DuDuClaw's own state vocabulary onto the vendored
 * `ThinkingOrb` mds primitive (`components/mds/thinking-orb.tsx` — a local
 * rewrite of the `thinking-orbs` MIT package's algorithms, not an npm
 * dependency; see that file's header for the full attribution) and owns
 * this layer's i18n label resolution. The canvas animation itself, the
 * reduced-motion gate, and the static motion-free fallback all live in the
 * primitive — this wrapper is purely a domain-vocabulary + label adapter.
 */
import { useIntl } from 'react-intl';
import { ThinkingOrb, type ThinkingOrbSize, type ThinkingOrbState } from '@/components/mds/thinking-orb';

/**
 * DuDuClaw's own chat/tool-execution state vocabulary (see
 * `wiki/duduclaw-kb/thinking-orbs-animations` mapping table). Kept distinct
 * from `ThinkingOrbState` so call sites reason about DuDuClaw concepts, not
 * the underlying animation's names.
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

const STATE_TO_ORB: Record<ThinkingState, { orb: ThinkingOrbState; size: ThinkingOrbSize }> = {
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

export interface ThinkingOrbIndicatorProps {
  /** Which DuDuClaw chat/tool state to show. */
  state: ThinkingState;
  /** Override the preset size (defaults to the size tuned for `state`). */
  size?: ThinkingOrbSize;
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
 * Renders the tuned orb animation for a DuDuClaw thinking/tool-execution
 * state (or the primitive's static motion-free glyph under reduced motion).
 */
export function ThinkingOrbIndicator({
  state,
  size,
  label,
  className,
  decorative = false,
}: ThinkingOrbIndicatorProps) {
  const intl = useIntl();
  const preset = STATE_TO_ORB[state];
  const resolvedSize = size ?? preset.size;
  const resolvedLabel =
    label ??
    intl.formatMessage({ id: `chat.thinking.${state}`, defaultMessage: DEFAULT_LABELS[state] });

  return (
    <ThinkingOrb
      state={preset.orb}
      size={resolvedSize}
      label={resolvedLabel}
      className={className}
      decorative={decorative}
    />
  );
}
