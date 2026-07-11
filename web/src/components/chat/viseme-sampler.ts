import { VISEMES, lerpViseme, type VisemeShape } from '@/components/mascot';

/**
 * Viseme sampling for the streaming reply (V7 / T7.1).
 *
 * The gateway streams the reply as text chunks (`assistant_chunk`), not
 * phonemes, so the mouth is driven by the *cadence and shape of arriving chunks*
 * rather than true lip-sync. Each chunk picks a target mouth shape from its last
 * meaningful character (latin vowels map to their obvious viseme; CJK / other
 * scripts fan out deterministically across the open vowels by code point) and we
 * lerp the current mouth toward it. Longer chunks open a little wider. A chunk
 * that is empty or pure punctuation/whitespace collapses back toward REST.
 *
 * Pure and deterministic — no timers, no DOM — so it is trivially unit-testable
 * and the store can call it synchronously on every chunk. The "pause → REST"
 * behaviour (silence between chunks) is owned by the store's idle timer, not
 * here.
 */

/** Neutral resting mouth — re-exported so callers seed without reaching mascot. */
export const REST_VISEME: VisemeShape = VISEMES.REST;

/** Open-vowel cycle used for scripts without latin vowels (mostly CJK). */
const VOWEL_CYCLE: readonly VisemeShape[] = [
  VISEMES.A,
  VISEMES.E,
  VISEMES.I,
  VISEMES.O,
  VISEMES.U,
];

/** Last non-space character of a chunk, or '' when there is none. */
function lastMeaningfulChar(chunk: string): string {
  const trimmed = chunk.replace(/\s+$/u, '');
  return trimmed.length > 0 ? trimmed[trimmed.length - 1] : '';
}

/** Map a single character to the mouth shape it most resembles. */
function targetFor(ch: string): VisemeShape {
  if (ch === '') return VISEMES.REST;
  const lower = ch.toLowerCase();
  switch (lower) {
    case 'a':
      return VISEMES.A;
    case 'e':
      return VISEMES.E;
    case 'i':
    case 'y':
      return VISEMES.I;
    case 'o':
      return VISEMES.O;
    case 'u':
    case 'w':
      return VISEMES.U;
    case 'm':
    case 'b':
    case 'p':
      return VISEMES.M;
    default:
      break;
  }
  // Punctuation / terminal marks → close the mouth (end of a clause reads as a
  // beat of silence).
  if (/[\s.,!?;:—…、。，！？；：)\]}"'`]/u.test(ch)) return VISEMES.REST;
  // Everything else (CJK, digits, other scripts): fan out over the open vowels
  // by code point so successive glyphs visibly move the mouth.
  const code = ch.codePointAt(0) ?? 0;
  return VOWEL_CYCLE[code % VOWEL_CYCLE.length];
}

/**
 * Advance the mouth one chunk. Returns the new {openness,width} shape.
 *
 * @param prev  the current mouth shape (seed with REST_VISEME on turn start).
 * @param chunk the freshly-arrived text chunk.
 */
export function sampleViseme(prev: VisemeShape, chunk: string): VisemeShape {
  const ch = lastMeaningfulChar(chunk);
  const target = targetFor(ch);
  // Longer chunks land closer to the target (bigger mouth movement); tiny
  // chunks barely nudge it so single-char streaming still animates smoothly.
  const t = Math.min(0.85, 0.4 + chunk.length * 0.08);
  return lerpViseme(prev, target, t);
}
