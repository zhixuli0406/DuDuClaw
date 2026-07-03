/**
 * Dependency-free fuzzy subsequence matcher for the command palette.
 *
 * Scores how well a short `query` matches a `target` label, and returns the
 * matched character indices so the UI can highlight them. Designed for small
 * candidate sets (dozens of nav items + actions), so clarity beats micro-opt.
 *
 * Scoring favours (highest → lowest): exact substring, word-boundary hits,
 * consecutive runs, and earlier matches. CJK-friendly: matching is codepoint
 * based (via spread), so Chinese labels match by character, and a Latin query
 * still matches Latin keywords attached to a CJK label.
 */

export interface FuzzyResult {
  /** Higher is better. Callers sort descending; ties fall back to input order. */
  readonly score: number;
  /** Indices into the target's codepoint array that matched the query. */
  readonly indices: readonly number[];
}

const SCORE_EXACT = 1_000_000;
const SCORE_PREFIX = 500_000;
const SCORE_WORD_START = 80;
const SCORE_CONSECUTIVE = 40;
const SCORE_MATCH = 10;
const PENALTY_LEADING_GAP = 3; // per skipped char before the first match
const PENALTY_GAP = 1; // per skipped char between matches

function isWordBoundary(prev: string | undefined, ch: string): boolean {
  if (prev === undefined) return true;
  // Latin camelCase / non-alphanumeric separators start a new "word".
  const sep = /[\s\-_/.·、，。()[\]{}:]/.test(prev);
  const camel = prev === prev.toLowerCase() && ch === ch.toUpperCase() && ch !== ch.toLowerCase();
  return sep || camel;
}

/**
 * Match `query` against `target`. Returns `null` when `query` is not a
 * subsequence of `target` (case-insensitive). Empty query matches with score 0.
 */
export function fuzzyMatch(query: string, target: string): FuzzyResult | null {
  const q = query.trim();
  if (q === '') return { score: 0, indices: [] };

  const targetChars = [...target];
  const lowerTarget = target.toLowerCase();
  const lowerQuery = q.toLowerCase();

  // Fast paths: exact / prefix / substring get a large, well-ordered bonus so
  // literal typing always floats to the top.
  const subIdx = lowerTarget.indexOf(lowerQuery);
  if (subIdx !== -1) {
    const runLen = [...lowerQuery].length;
    const start = [...lowerTarget.slice(0, subIdx)].length;
    const indices = Array.from({ length: runLen }, (_, i) => start + i);
    let score: number;
    if (lowerTarget === lowerQuery) score = SCORE_EXACT;
    else if (subIdx === 0) score = SCORE_PREFIX;
    else score = SCORE_PREFIX - start * PENALTY_GAP;
    return { score, indices };
  }

  // Subsequence walk with contextual bonuses.
  const lowerTargetChars = [...lowerTarget];
  const lowerQueryChars = [...lowerQuery];
  let qi = 0;
  let score = 0;
  let prevMatch = -2;
  const indices: number[] = [];

  for (let ti = 0; ti < lowerTargetChars.length && qi < lowerQueryChars.length; ti++) {
    if (lowerTargetChars[ti] !== lowerQueryChars[qi]) continue;

    indices.push(ti);
    score += SCORE_MATCH;
    if (isWordBoundary(targetChars[ti - 1], targetChars[ti])) score += SCORE_WORD_START;
    if (prevMatch === ti - 1) score += SCORE_CONSECUTIVE;
    else if (prevMatch === -2) score -= ti * PENALTY_LEADING_GAP;
    else score -= (ti - prevMatch - 1) * PENALTY_GAP;

    prevMatch = ti;
    qi++;
  }

  if (qi < lowerQueryChars.length) return null; // query not fully consumed
  return { score, indices };
}

/**
 * Split a label into `{ text, hit }` segments for highlight rendering, given
 * the matched codepoint indices. Keeps CJK-safe by operating on the codepoint
 * array rather than raw string indices.
 */
export function highlightSegments(
  label: string,
  indices: readonly number[]
): ReadonlyArray<{ text: string; hit: boolean }> {
  if (indices.length === 0) return [{ text: label, hit: false }];
  const chars = [...label];
  const hitSet = new Set(indices);
  const segments: { text: string; hit: boolean }[] = [];
  let current = '';
  let currentHit = hitSet.has(0);

  chars.forEach((ch, i) => {
    const hit = hitSet.has(i);
    if (hit === currentHit) {
      current += ch;
    } else {
      if (current) segments.push({ text: current, hit: currentHit });
      current = ch;
      currentHit = hit;
    }
  });
  if (current) segments.push({ text: current, hit: currentHit });
  return segments;
}
