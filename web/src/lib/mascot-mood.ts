/**
 * mascot-mood — pure mood-derivation logic for the DuDu mascot (V9 / §7.2).
 * Kept free of React/DOM so it is trivially unit-testable and reusable from
 * both the store and the presentational component. `moodExpression` now maps a
 * mood onto a DuDu `face` preset (the old emoji output is retired with the
 * emoji mascot).
 */

import type { DuduFace } from '@/components/mascot/faces';

export type MascotMood = 'relaxed' | 'focused' | 'alert' | 'poke';

export interface MascotMoodInput {
  /** Total number of AI staff known to the dashboard. */
  total: number;
  /** Count currently `active`. */
  active: number;
  /** Count in an error state (reserved — `AgentInfo.status` has no 'error'
   *  member today; callers pass 0 until an error signal exists upstream). */
  error: number;
  /** Unified "needs me" inbox count (`useApprovalsStore.pendingCount`). */
  inbox: number;
}

/**
 * Precedence (highest first): alert > poke > focused > relaxed.
 * An error always wins — it is the most urgent signal. Otherwise a
 * non-empty inbox asks for attention before "just busy working" does.
 */
export function computeMood(input: MascotMoodInput): MascotMood {
  if (input.error > 0) return 'alert';
  if (input.inbox > 0) return 'poke';
  if (input.active > 0) return 'focused';
  return 'relaxed';
}

export interface MascotExpression {
  /** The DuDu face preset this mood renders as. */
  face: DuduFace;
  /** i18n message id for the mood's one-line label. */
  labelId: string;
}

const EXPRESSIONS: Record<MascotMood, MascotExpression> = {
  relaxed: { face: 'idle', labelId: 'mascot.mood.relaxed' },
  focused: { face: 'writing', labelId: 'mascot.mood.focused' },
  alert: { face: 'concerned', labelId: 'mascot.mood.alert' },
  poke: { face: 'curious', labelId: 'mascot.mood.poke' },
};

export function moodExpression(mood: MascotMood): MascotExpression {
  return EXPRESSIONS[mood];
}
