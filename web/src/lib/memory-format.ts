/**
 * Human-friendly rendering helpers for raw memory entries.
 *
 * The prediction engine (System 1.5, Moderate errors) stores episodic memories
 * as a fixed English telemetry string — e.g.
 *   "Prediction deviation: expected satisfaction 0.75, inferred 0.61
 *    (delta 0.14). Topic surprise: 1.00. Corrections: yes. Follow-ups: no.
 *    Topics: 報價, 合約."
 * Shown verbatim this is meaningless to end users, so the Memory page parses it
 * into structured fields and renders a localized "learning signal" card instead.
 * Non-matching content is left untouched.
 *
 * The trailing `Topics: …` / `Expected topic: …` sentences are both optional —
 * older entries were written before the conversation's topics were recorded,
 * so `topics` is simply empty for them (never fabricated).
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
  /**
   * What the conversation was actually about — up to 3 keywords extracted
   * from the user's messages, plus the model's predicted topic when that
   * differs from all of them. Empty when the entry predates topic tracking
   * (nothing to show; never fabricated).
   */
  topics: string[];
}

// Mirrors the exact `format!` in crates/duduclaw-gateway/src/prediction/router.rs
// (see `format_moderate_telemetry`) — keep the two in sync.
const PREDICTION_RE =
  /^Prediction deviation: expected satisfaction (\d+(?:\.\d+)?), inferred (\d+(?:\.\d+)?) \(delta (-?\d+(?:\.\d+)?)\)\. Topic surprise: (\d+(?:\.\d+)?)\. Corrections: (yes|no)\. Follow-ups: (yes|no)\.?(?: Topics: ([^.]+)\.?)?(?: Expected topic: ([^.]+)\.?)?$/;

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

  const extractedTopics = m[7]
    ? m[7]
        .split(',')
        .map((t) => t.trim())
        .filter((t) => t.length > 0)
    : [];
  const expectedTopic = m[8]?.trim();
  const topics =
    expectedTopic && !extractedTopics.includes(expectedTopic)
      ? [...extractedTopics, expectedTopic]
      : extractedTopics;

  return {
    expected,
    inferred,
    delta,
    surprise,
    corrected: m[5] === 'yes',
    followUp: m[6] === 'yes',
    topics,
  };
}

/** Convert a 0–1 ratio to a rounded whole percentage (clamped to 0–100). */
export function toPercent(ratio: number): number {
  return Math.max(0, Math.min(100, Math.round(ratio * 100)));
}

/** Sentence terminators we are willing to cut a summary on (CJK + ASCII). */
const SENTENCE_END = ['。', '！', '？', '；', '\n', '. ', '! ', '? ', '; '];

/**
 * Lazily-built grapheme segmenter, or `null` where `Intl.Segmenter` is absent.
 * `undefined` = not probed yet.
 */
let graphemeSegmenter: Intl.Segmenter | null | undefined;

/**
 * Split a string into user-perceived characters.
 *
 * Codepoints are not a safe unit to cut on: a 🇹🇼 flag is two regional
 * indicators and 👨‍👩‍👧‍👦 is seven codepoints joined by ZWJ, so a budget that
 * lands mid-sequence would leave half a flag or a stray lone emoji behind.
 * `Intl.Segmenter` with grapheme granularity keeps those clusters whole.
 *
 * Where `Intl.Segmenter` is unavailable (very old runtimes) this falls back to
 * codepoints via `Array.from` — still never splitting a surrogate pair, but a
 * composed emoji sequence may be cut at an internal boundary.
 */
function graphemes(input: string): string[] {
  if (graphemeSegmenter === undefined) {
    graphemeSegmenter =
      typeof Intl !== 'undefined' && typeof Intl.Segmenter === 'function'
        ? new Intl.Segmenter(undefined, { granularity: 'grapheme' })
        : null;
  }
  if (!graphemeSegmenter) return Array.from(input);
  return Array.from(graphemeSegmenter.segment(input), (s) => s.segment);
}

/**
 * Collapse a memory's stored content into the summary the list row shows
 * (WP5a / D6: "a list of memories, expand for detail").
 *
 * Rules, in order: flatten every non-empty line into one string joined by
 * `·`; if it exceeds the budget, cut at the last sentence terminator that
 * fits; otherwise hard-cut and append an ellipsis. `maxChars` counts grapheme
 * clusters (see {@link graphemes}), never bytes or UTF-16 indices, so CJK,
 * flags and ZWJ emoji sequences all survive the cut intact.
 *
 * WP15: this used to keep only the *first* line, which hid the substance of
 * any memory written with a heading ("客戶資料更新 / 公司: … / 電話: …" showed
 * just the heading). Joining the lines keeps the identifying detail on the
 * row; the full text is still one click away in the detail panel.
 */
export function summarizeMemory(content: string, maxChars = 90): string {
  const lines = content
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  const line = (lines.length > 0 ? lines.join(' · ') : content.trim()).replace(/\s+/g, ' ');
  const chars = graphemes(line);
  if (chars.length <= maxChars) return line;

  const head = chars.slice(0, maxChars).join('');
  // Prefer a sentence boundary, but only if it keeps a useful amount of text.
  let cut = -1;
  for (const t of SENTENCE_END) {
    const i = head.lastIndexOf(t);
    if (i > cut) cut = i + t.trimEnd().length;
  }
  if (cut >= Math.floor(maxChars * 0.4)) return head.slice(0, cut);
  return `${head.trimEnd()}…`;
}

/**
 * Cognitive layer → i18n message id, in end-user words rather than the internal
 * `episodic` / `semantic` vocabulary (WP5a item 3). Unknown or missing layers
 * return `null` so the row simply shows no chip.
 */
export function memoryLayerMessageId(layer?: string): string | null {
  switch (layer?.toLowerCase()) {
    case 'episodic':
      return 'memory.layer.episodic';
    case 'semantic':
      return 'memory.layer.semantic';
    case 'procedural':
      return 'memory.layer.procedural';
    default:
      return null;
  }
}

/**
 * Where a memory came from → i18n message id, again in plain words ("noted
 * from a conversation", not "distillation pipeline"). Anything unrecognized
 * falls back to the conversation wording, which is true for every write path
 * that does not stamp an explicit origin.
 */
export function memorySourceMessageId(sourceEvent?: string, tags: readonly string[] = []): string {
  const key = sourceEvent?.toLowerCase() ?? '';
  const byEvent: Readonly<Record<string, string>> = {
    footprint_distill: 'memory.source.footprint',
    prediction_episodic: 'memory.source.prediction',
    reflexion_consolidation: 'memory.source.reflexion',
    persona_suppression_induction: 'memory.source.profile',
    user_profile: 'memory.source.profile',
    user_profile_consolidation: 'memory.source.profile',
    decision_capture: 'memory.source.decision',
    decision_resolve: 'memory.source.decision',
    agent_mood: 'memory.source.mood',
  };
  if (byEvent[key]) return byEvent[key];

  const byTag: Readonly<Record<string, string>> = {
    'footprint-distill': 'memory.source.footprint',
    'user-profile': 'memory.source.profile',
    'profile-summary': 'memory.source.profile',
    reflexion: 'memory.source.reflexion',
    'probation-rule': 'memory.source.reflexion',
    'retired-rule': 'memory.source.reflexion',
    decision: 'memory.source.decision',
  };
  for (const tag of tags) {
    const hit = byTag[tag.toLowerCase()];
    if (hit) return hit;
  }
  return 'memory.source.conversation';
}
