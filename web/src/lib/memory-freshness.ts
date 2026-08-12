/**
 * Memory freshness — the plain-language layer over the gateway's decay figures.
 *
 * The backend hands every memory row a `retrievability` (0–1) and a
 * `stability_days`. Neither number is ever shown to a user: this module maps
 * them onto four states people can act on — 新鮮 / 穩固 / 逐漸淡忘 / 即將歸檔 —
 * and supplies the curve maths the detail view draws.
 *
 * Why a shared module rather than inline logic: the badge, the legend, the
 * distribution chart and the decay curve must all agree on where the boundaries
 * are, and the boundaries are also asserted server-side
 * (`MEMORY_FRESHNESS_BANDS` in the gateway). One table here, one table there,
 * and a test on each side pinning the same four numbers.
 *
 * Colour note: the four states are a *status* scale (good → about to be filed
 * away), not a categorical series, so they wear the Calm Glass semantic tokens
 * and always ship with an icon and a written label. Colour is redundant
 * reinforcement everywhere it appears — never the only channel — which is what
 * keeps the two green states readable for colour-blind users.
 */

/** Stable wire keys, ordered freshest → faintest. */
export const FRESHNESS_IDS = ['fresh', 'stable', 'fading', 'archiving'] as const;

export type FreshnessId = (typeof FRESHNESS_IDS)[number];

/**
 * Inclusive lower bound per state. Mirrors the gateway's
 * `MEMORY_FRESHNESS_BANDS`; changing one without the other is a bug.
 */
export const FRESHNESS_BANDS: ReadonlyArray<{ id: FreshnessId; min: number }> = [
  { id: 'fresh', min: 0.7 },
  { id: 'stable', min: 0.4 },
  { id: 'fading', min: 0.15 },
  { id: 'archiving', min: 0 },
];

/**
 * Retrievability at which the platform files a memory away — the gateway sends
 * its live value on `memory.decay_overview`; this is the fallback for surfaces
 * that only have a single row to work with.
 */
export const DEFAULT_ARCHIVE_THRESHOLD = 0.05;

/** Which state a retrievability value falls in. `null` for a missing value. */
export function freshnessBand(retrievability: number | null | undefined): FreshnessId | null {
  if (typeof retrievability !== 'number' || !Number.isFinite(retrievability)) return null;
  const clamped = Math.min(1, Math.max(0, retrievability));
  return FRESHNESS_BANDS.find((b) => clamped >= b.min)?.id ?? 'archiving';
}

/** i18n key for a state's short label (the badge text). */
export function freshnessLabelId(id: FreshnessId): string {
  return `memory.freshness.${id}`;
}

/** i18n key for a state's plain-language explanation (the tooltip body). */
export function freshnessHintId(id: FreshnessId): string {
  return `memory.freshness.${id}.hint`;
}

/**
 * Per-state presentation. `swatch` is a Tailwind *fill* class for SVG marks;
 * `opacity` separates the two green states by lightness so they stay distinct
 * without spending a second hue. `tone` colours the icon only — badge text
 * keeps the high-contrast foreground token so the label never depends on a
 * light semantic colour being readable.
 */
export interface FreshnessStyle {
  /** Icon tint + legend swatch colour. */
  tone: string;
  /** Badge chrome (border + wash). */
  badge: string;
  /** SVG fill class for chart marks. */
  swatch: string;
  /** SVG fill opacity, separating the two healthy states. */
  opacity: number;
}

export const FRESHNESS_STYLES: Record<FreshnessId, FreshnessStyle> = {
  fresh: {
    tone: 'text-success',
    badge: 'border-success/35 bg-success/10',
    swatch: 'fill-success',
    opacity: 1,
  },
  stable: {
    tone: 'text-success/75',
    badge: 'border-success/25 bg-success/6',
    swatch: 'fill-success',
    // 0.65, not lower: blending toward the surface costs chroma, and below
    // roughly this point the mark stops reading as green at all and collapses
    // against the amber row for full-colour and colour-blind readers alike.
    // Measured with the dataviz palette validator against both surfaces.
    opacity: 0.65,
  },
  fading: {
    tone: 'text-warning',
    badge: 'border-warning/35 bg-warning/10',
    swatch: 'fill-warning',
    opacity: 1,
  },
  archiving: {
    tone: 'text-destructive',
    badge: 'border-destructive/35 bg-destructive/10',
    swatch: 'fill-destructive',
    opacity: 1,
  },
};

/**
 * The forgetting curve itself: how retrievable a memory is `days` after it was
 * last recalled, given its stability. Same expression as the gateway's
 * `ebbinghaus_retrievability`, so the curve passes exactly through the point
 * the badge reports.
 */
export function retrievabilityAt(days: number, stabilityDays: number): number {
  if (!(stabilityDays > 0)) return 0;
  return Math.exp(-Math.max(0, days) / stabilityDays);
}

/** Days until a memory decays to `target` retrievability. */
export function daysToReach(target: number, stabilityDays: number): number {
  if (!(stabilityDays > 0) || !(target > 0) || target >= 1) return 0;
  return -stabilityDays * Math.log(target);
}

/**
 * Days elapsed since a memory was last recalled — falling back to when it was
 * recorded, which is the anchor the backend uses too.
 */
export function daysSinceRecall(
  entry: { timestamp: string; last_accessed?: string | null },
  now: number = Date.now(),
): number {
  const anchor = entry.last_accessed ?? entry.timestamp;
  const parsed = Date.parse(anchor);
  if (Number.isNaN(parsed)) return 0;
  return Math.max(0, (now - parsed) / 86_400_000);
}
