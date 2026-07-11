/**
 * Mouth-shape primitives for the DuDu mascot (V9 / §7.1). A viseme is an
 * `{openness, width}` pair the renderer turns into an SVG path centred on the
 * mouth area. Speaking callers feed a stream of visemes on their own rhythm
 * (token cadence) so the mouth tracks the reply.
 *
 * Pure — no React, no DOM — so it is trivially unit-testable and shared by the
 * face-preset table.
 */

export type VisemeId = 'REST' | 'A' | 'E' | 'I' | 'O' | 'U' | 'M';

export interface VisemeShape {
  /** 0 = closed, 1 = fully open vertically. */
  openness: number;
  /** 0 = pursed (O/U), 1 = wide (E/I). */
  width: number;
}

/** Six mouth targets plus REST — enough shape variety to read as speech. */
export const VISEMES: Record<VisemeId, VisemeShape> = {
  REST: { openness: 0, width: 0.35 },
  A: { openness: 0.95, width: 0.6 },
  E: { openness: 0.45, width: 1.0 },
  I: { openness: 0.3, width: 0.85 },
  O: { openness: 0.75, width: 0.2 },
  U: { openness: 0.4, width: 0.05 },
  M: { openness: 0, width: 0.4 },
};

/** Anchor point for the mouth oval in the 100×100 DuDu viewBox. */
const CX = 50;
const CY = 59;

/** The resting smile used when a mouth is effectively closed. */
export const REST_SMILE_PATH = 'M44,57.5 Q50,63 56,57.5 Q50,60.5 44,57.5 Z';

/** Linear interpolation between two viseme shapes (clamped). */
export function lerpViseme(a: VisemeShape, b: VisemeShape, t: number): VisemeShape {
  const k = Math.max(0, Math.min(1, t));
  return {
    openness: a.openness + (b.openness - a.openness) * k,
    width: a.width + (b.width - a.width) * k,
  };
}

/**
 * Build the SVG `d` attribute for a mouth shape. When `openness` collapses we
 * fall back to the resting smile so the face never looks slack.
 */
export function visemePath(shape: VisemeShape): string {
  if (shape.openness < 0.05) return REST_SMILE_PATH;
  const halfW = 5 + shape.width * 7;
  const halfH = 1.5 + shape.openness * 6;
  const left = CX - halfW;
  const right = CX + halfW;
  const top = CY - halfH;
  const bot = CY + halfH;
  return `M${left},${CY} Q${CX},${top} ${right},${CY} Q${CX},${bot} ${left},${CY} Z`;
}
