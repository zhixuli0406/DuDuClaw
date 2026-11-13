/**
 * v1.82 (H1): make characters that hide or reorder text visible.
 *
 * An approval view must show what will run, character for character. Bidi
 * controls (e.g. U+202E RIGHT-TO-LEFT OVERRIDE) can make text read differently
 * from what it is; zero-width / invisible characters and Unicode tag or
 * variation-selector runs can carry instructions a human never sees; C0/C1
 * control characters can do either. Each such character is rendered as a
 * visible `\u{XXXX}` escape. Line feeds and tabs are kept (they are layout,
 * and the content is shown in a pre-wrapped block); everything else in the
 * classes below is escaped — including CR.
 *
 * The one exception: U+FE0F (emoji presentation selector) directly after an
 * extended pictographic character is ordinary emoji spelling and is kept.
 */
export interface VisibleText {
  /** The input with every hidden/control character replaced by `\u{XXXX}`. */
  text: string;
  /** How many characters were escaped. */
  escaped: number;
}

const PICTOGRAPHIC = /\p{Extended_Pictographic}/u;

/** Whether `cp` is a character this view must never render raw. */
export function isHiddenOrControl(cp: number): boolean {
  return (
    (cp <= 0x1f && cp !== 0x0a && cp !== 0x09) || // C0 (keep LF, TAB)
    (cp >= 0x7f && cp <= 0x9f) || // DEL + C1
    cp === 0x00ad || // soft hyphen
    cp === 0x034f || // combining grapheme joiner
    cp === 0x061c || // Arabic letter mark (bidi)
    cp === 0x115f || cp === 0x1160 || // Hangul choseong/jungseong fillers
    cp === 0x17b4 || cp === 0x17b5 || // Khmer inherent vowels
    cp === 0x180e || // Mongolian vowel separator
    (cp >= 0x200b && cp <= 0x200f) || // ZWSP, ZWNJ, ZWJ, LRM, RLM
    (cp >= 0x2028 && cp <= 0x202e) || // LS, PS, LRE, RLE, PDF, LRO, RLO
    (cp >= 0x2060 && cp <= 0x206f) || // WJ, invisible operators, LRI…PDI, deprecated format
    cp === 0x3164 || // Hangul filler
    (cp >= 0xfe00 && cp <= 0xfe0f) || // variation selectors
    cp === 0xfeff || // BOM / ZWNBSP
    cp === 0xffa0 || // halfwidth Hangul filler
    (cp >= 0xfff9 && cp <= 0xfffb) || // interlinear annotation
    (cp >= 0x1bca0 && cp <= 0x1bca3) || // shorthand format controls
    (cp >= 0x1d173 && cp <= 0x1d17a) || // musical format controls
    (cp >= 0xe0000 && cp <= 0xe007f) || // tag characters
    (cp >= 0xe0100 && cp <= 0xe01ef) // variation selectors supplement
  );
}

/** Format one code point as a visible escape: `\u{202E}`. */
export function escapeCodePoint(cp: number): string {
  return `\\u{${cp.toString(16).toUpperCase().padStart(4, '0')}}`;
}

/** Replace every hidden/control character of `input` with a visible escape. */
export function visibleEscapes(input: string): VisibleText {
  let text = '';
  let escaped = 0;
  let previous = '';
  for (const ch of input) {
    const cp = ch.codePointAt(0) ?? 0;
    const emojiSelector = cp === 0xfe0f && previous !== '' && PICTOGRAPHIC.test(previous);
    if (isHiddenOrControl(cp) && !emojiSelector) {
      text += escapeCodePoint(cp);
      escaped += 1;
    } else {
      text += ch;
    }
    previous = ch;
  }
  return { text, escaped };
}
