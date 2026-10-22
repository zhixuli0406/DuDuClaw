/**
 * Thinking display for one WebChat turn — pure data + reducers (no React, no
 * store). The gateway sends `thinking` frames (already masked and truncated
 * server-side; see `webchat.rs` `ChatMessage::Thinking`) and, on
 * `assistant_done`, an optional `thinking` summary `{tokens, blocks, kinds}`.
 *
 * The visible timer never depends on those frames: a turn starts the moment
 * the user sends (`startTurnThinking`), so every runtime shows "thinking… N s"
 * even when its provider reports nothing. Thinking text lives only in memory,
 * attached to the reply it belongs to; nothing here persists it.
 */

/** Wire `kind` values. Anything else is treated as `omitted` (no text). */
export type ThinkingKind = 'summary' | 'raw' | 'progress' | 'redacted' | 'omitted';

const KNOWN_KINDS: ReadonlySet<string> = new Set(['summary', 'raw', 'progress', 'redacted', 'omitted']);

/** Block id the gateway uses for whole-turn token usage (not a display block). */
export const TURN_USAGE_BLOCK = 'turn';

/** One thinking block of a turn, folded from its start/update/end frames. */
export interface ThinkingBlock {
  readonly id: string;
  readonly kind: ThinkingKind;
  /** Masked text snapshot (only for summary/raw/progress viewers). */
  readonly text?: string;
  /** One-line progress (≤ 80 chars, masked). */
  readonly line?: string;
  /** Gateway clock, unix ms. */
  readonly startedAt?: number;
  /** Only once the block ended. */
  readonly durationMs?: number;
  readonly tokens?: number;
  readonly truncated: boolean;
  readonly done: boolean;
}

/** Everything the UI knows about one turn's thinking. */
export interface TurnThinking {
  /** Browser clock (ms) when the user sent the message. */
  readonly startedAt: number;
  /** Browser clock (ms) when the reply arrived; absent while live. */
  readonly endedAt?: number;
  readonly blocks: readonly ThinkingBlock[];
  /** Whole-turn token usage (`block: "turn"`), preferred over block sums. */
  readonly turnTokens?: number;
  /** Raw open-model reasoning was withheld because redaction is off. */
  readonly rawNeedsRedaction: boolean;
}

/** The parsed wire shape of a `thinking` frame. */
export interface ThinkingFrame {
  readonly phase: string;
  readonly block: string;
  readonly kind: string;
  readonly text?: string;
  readonly line?: string;
  readonly started_at?: number;
  readonly duration_ms?: number;
  readonly tokens?: number;
  readonly truncated?: boolean;
  readonly raw_needs_redaction?: boolean;
}

/** A fresh turn, started at `now` (the moment the user pressed send). */
export function startTurnThinking(now: number): TurnThinking {
  return { startedAt: now, blocks: [], rawNeedsRedaction: false };
}

function num(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) && v >= 0 ? v : undefined;
}

function str(v: unknown): string | undefined {
  return typeof v === 'string' && v.length > 0 ? v : undefined;
}

function normaliseKind(kind: unknown): ThinkingKind {
  return typeof kind === 'string' && KNOWN_KINDS.has(kind) ? (kind as ThinkingKind) : 'omitted';
}

/** Only these kinds may carry text on screen. */
function kindShowsText(kind: ThinkingKind): boolean {
  return kind === 'summary' || kind === 'raw' || kind === 'progress';
}

/** Merge one frame into a block (new or existing). Pure. */
function mergeBlock(prev: ThinkingBlock | undefined, frame: ThinkingFrame, ending: boolean): ThinkingBlock {
  // Once refused, a block stays refused even if a stray later frame says otherwise.
  const kind: ThinkingKind = prev?.kind === 'redacted' ? 'redacted' : normaliseKind(frame.kind);
  const showsText = kindShowsText(kind);
  // A `redacted` frame for a block that already showed text means the
  // outbound checks refused it: the text on screen must go (sticky).
  const text = showsText ? (str(frame.text) ?? prev?.text) : undefined;
  const line = showsText ? (str(frame.line) ?? prev?.line) : undefined;
  const startedAt = num(frame.started_at) ?? prev?.startedAt;
  const durationMs = num(frame.duration_ms) ?? prev?.durationMs;
  const tokens = num(frame.tokens) ?? prev?.tokens;
  return {
    id: frame.block,
    kind,
    ...(text !== undefined ? { text } : {}),
    ...(line !== undefined ? { line } : {}),
    ...(startedAt !== undefined ? { startedAt } : {}),
    ...(durationMs !== undefined ? { durationMs } : {}),
    ...(tokens !== undefined ? { tokens } : {}),
    truncated: showsText && (frame.truncated === true || (prev?.truncated ?? false)),
    done: ending || (prev?.done ?? false),
  };
}

/**
 * Fold one `thinking` frame into the turn (pure — exported for tests).
 *
 *  - `block: "turn"` only records whole-turn token usage.
 *  - `start` adds the block (a repeated start for a known id replaces it).
 *  - `update` replaces the block's snapshot by id (creates it if the start
 *    was missed — frames are best-effort).
 *  - `end` marks the block done with its final text / duration / tokens.
 *  - `kind: "redacted"` (any phase) clears that block's text and line.
 *  - unknown phases and frames without a block id are ignored; never throws.
 */
export function applyThinking(turn: TurnThinking, frame: ThinkingFrame): TurnThinking {
  if (typeof frame.block !== 'string' || frame.block.length === 0) return turn;
  const rawNeedsRedaction = turn.rawNeedsRedaction || frame.raw_needs_redaction === true;
  if (frame.block === TURN_USAGE_BLOCK) {
    const tokens = num(frame.tokens);
    return { ...turn, rawNeedsRedaction, ...(tokens !== undefined ? { turnTokens: tokens } : {}) };
  }
  if (frame.phase !== 'start' && frame.phase !== 'update' && frame.phase !== 'end') return turn;
  const idx = turn.blocks.findIndex((b) => b.id === frame.block);
  const prev = frame.phase === 'start' || idx < 0 ? undefined : turn.blocks[idx];
  const block = mergeBlock(prev, frame, frame.phase === 'end');
  const blocks =
    idx < 0 ? [...turn.blocks, block] : turn.blocks.map((b, i) => (i === idx ? block : b));
  return { ...turn, blocks, rawNeedsRedaction };
}

/** The `assistant_done.thinking` summary (`{tokens, blocks, kinds}`). */
export interface ThinkingDoneMeta {
  readonly tokens?: number;
  readonly blocks?: number;
  readonly kinds?: readonly string[];
}

/** Validate the optional `assistant_done.thinking` value. Pure. */
export function parseThinkingDoneMeta(raw: unknown): ThinkingDoneMeta | undefined {
  if (raw === null || typeof raw !== 'object') return undefined;
  const r = raw as Record<string, unknown>;
  const kinds = Array.isArray(r.kinds) ? r.kinds.filter((k): k is string => typeof k === 'string') : undefined;
  return {
    ...(num(r.tokens) !== undefined ? { tokens: num(r.tokens) } : {}),
    ...(num(r.blocks) !== undefined ? { blocks: num(r.blocks) } : {}),
    ...(kinds ? { kinds } : {}),
  };
}

/**
 * Close the turn when its reply arrives (pure — exported for tests). The
 * gateway's own count fills in tokens when no frame carried them. A turn
 * whose frames never arrived (display off, older gateway, a runtime that
 * reports nothing) still keeps its wall time.
 */
export function finishTurnThinking(turn: TurnThinking, endedAt: number, meta?: ThinkingDoneMeta): TurnThinking {
  const turnTokens = turn.turnTokens ?? (meta?.tokens && meta.tokens > 0 ? meta.tokens : undefined);
  return {
    ...turn,
    endedAt: Math.max(endedAt, turn.startedAt),
    ...(turnTokens !== undefined ? { turnTokens } : {}),
  };
}

/** Whole seconds shown for a span, rounded up (never 0) so the live counter
 *  and the collapsed "thought for N s" read the same number. */
export function thinkingSeconds(startedAt: number, now: number): number {
  return Math.max(1, Math.ceil(Math.max(0, now - startedAt) / 1000));
}

/** Approximate tokens for the turn: whole-turn usage, else the block sum. */
export function thinkingTokens(turn: TurnThinking): number | undefined {
  if (turn.turnTokens !== undefined) return turn.turnTokens > 0 ? turn.turnTokens : undefined;
  const sum = turn.blocks.reduce((acc, b) => acc + (b.tokens ?? 0), 0);
  return sum > 0 ? sum : undefined;
}

/** `840` → `"840"`, `1240` → `"1.2k"`, `12000` → `"12k"`. */
export function formatApproxTokens(n: number): string {
  if (n < 1000) return String(Math.round(n));
  const k = n / 1000;
  const shown = k >= 10 ? Math.round(k).toString() : (Math.round(k * 10) / 10).toString();
  return `${shown}k`;
}

/** The newest progress line any block reported (the live status row). */
export function latestThinkingLine(turn: TurnThinking | null | undefined): string | undefined {
  if (!turn) return undefined;
  for (let i = turn.blocks.length - 1; i >= 0; i -= 1) {
    const line = turn.blocks[i].line;
    if (line) return line;
  }
  return undefined;
}

/** Blocks that have text to show when expanded. */
export function blocksWithText(turn: TurnThinking): readonly ThinkingBlock[] {
  return turn.blocks.filter((b) => kindShowsText(b.kind) && typeof b.text === 'string' && b.text.length > 0);
}
