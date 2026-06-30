/**
 * Human-friendly rendering helpers for raw memory entries.
 *
 * The prediction engine (System 1.5, Moderate errors) stores episodic memories
 * as a fixed English telemetry string — e.g.
 *   "Prediction deviation: expected satisfaction 0.75, inferred 0.61
 *    (delta 0.14). Topic surprise: 1.00. Corrections: yes. Follow-ups: no."
 * Shown verbatim this is meaningless to end users, so the Memory page parses it
 * into structured fields and renders a localized "learning signal" card instead.
 * Non-matching content is left untouched.
 */

export interface PredictionMemory {
  /** Model's expected satisfaction, 0–1. */
  expected: number;
  /** Satisfaction actually inferred from the interaction, 0–1. */
  inferred: number;
  /** expected − inferred (positive = we over-estimated). */
  delta: number;
  /** Topic novelty / surprise, 0–1. */
  surprise: number;
  /** The user corrected the agent when a smooth turn was predicted. */
  corrected: boolean;
  /** The user followed up unexpectedly. */
  followUp: boolean;
}

// Mirrors the exact `format!` in crates/duduclaw-gateway/src/prediction/router.rs.
const PREDICTION_RE =
  /^Prediction deviation: expected satisfaction (\d+(?:\.\d+)?), inferred (\d+(?:\.\d+)?) \(delta (-?\d+(?:\.\d+)?)\)\. Topic surprise: (\d+(?:\.\d+)?)\. Corrections: (yes|no)\. Follow-ups: (yes|no)\.?$/;

/**
 * Parse a prediction-deviation episodic memory into structured fields, or
 * `null` when `content` is an ordinary memory (caller falls back to raw text).
 */
export function parsePredictionMemory(content: string): PredictionMemory | null {
  const m = PREDICTION_RE.exec(content.trim());
  if (!m) return null;
  const expected = Number(m[1]);
  const inferred = Number(m[2]);
  const delta = Number(m[3]);
  const surprise = Number(m[4]);
  if ([expected, inferred, delta, surprise].some((n) => Number.isNaN(n))) return null;
  return {
    expected,
    inferred,
    delta,
    surprise,
    corrected: m[5] === 'yes',
    followUp: m[6] === 'yes',
  };
}

/** Convert a 0–1 ratio to a rounded whole percentage (clamped to 0–100). */
export function toPercent(ratio: number): number {
  return Math.max(0, Math.min(100, Math.round(ratio * 100)));
}
