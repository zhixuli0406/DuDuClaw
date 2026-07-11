/**
 * CJK-safe text helpers for the world stage. `duduclaw_core::truncate_*` is
 * server-side Rust; the front end can't call it, so we truncate by *grapheme*
 * (Intl.Segmenter when present, `Array.from` codepoints otherwise) — never by
 * raw string index, which would split a multi-byte CJK char or emoji.
 */

/** Count graphemes, preferring Intl.Segmenter for correct CJK/emoji clustering. */
function graphemes(text: string): string[] {
  const Seg = (Intl as unknown as { Segmenter?: typeof Intl.Segmenter }).Segmenter;
  if (Seg) {
    const seg = new Seg(undefined, { granularity: 'grapheme' });
    return Array.from(seg.segment(text), (s) => s.segment);
  }
  // Fallback: codepoint split (handles surrogate pairs, not full grapheme clusters).
  return Array.from(text);
}

/**
 * Truncate to at most `max` graphemes, appending an ellipsis when clipped.
 * `max` counts the visible glyphs, not the ellipsis.
 */
export function truncateGraphemes(text: string, max: number): string {
  const t = text.trim();
  if (max <= 0) return '';
  const g = graphemes(t);
  if (g.length <= max) return t;
  return g.slice(0, max).join('') + '…';
}
