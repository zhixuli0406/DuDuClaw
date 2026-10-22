import { describe, it, expect } from 'vitest';
import {
  applyThinking,
  blocksWithText,
  finishTurnThinking,
  formatApproxTokens,
  latestThinkingLine,
  parseThinkingDoneMeta,
  startTurnThinking,
  thinkingSeconds,
  thinkingTokens,
  type ThinkingFrame,
} from './chat-thinking';
import { CONVERSATION_FRAME_TYPES, IN_TURN_FRAME_TYPES, shouldRenderFrame } from './chat-frames';

const T0 = 1_000_000;

function fold(frames: ThinkingFrame[]) {
  return frames.reduce(applyThinking, startTurnThinking(T0));
}

describe('applyThinking reducer', () => {
  it('start adds a block, update replaces its snapshot by id, end marks it done', () => {
    const turn = fold([
      { phase: 'start', block: 't0', kind: 'summary', started_at: T0 + 10 },
      { phase: 'update', block: 't0', kind: 'summary', text: 'Comparing', line: 'Comparing two', tokens: 40 },
      { phase: 'update', block: 't0', kind: 'summary', text: 'Comparing both reports', tokens: 90 },
      { phase: 'end', block: 't0', kind: 'summary', text: 'Comparing both reports.', duration_ms: 8123, tokens: 120 },
    ]);
    expect(turn.blocks).toHaveLength(1);
    const [b] = turn.blocks;
    expect(b).toMatchObject({
      id: 't0',
      kind: 'summary',
      text: 'Comparing both reports.',
      line: 'Comparing two',
      startedAt: T0 + 10,
      durationMs: 8123,
      tokens: 120,
      done: true,
      truncated: false,
    });
  });

  it('keeps blocks in arrival order and does not mutate the input turn', () => {
    const start = startTurnThinking(T0);
    const a = applyThinking(start, { phase: 'start', block: 't0', kind: 'summary' });
    const b = applyThinking(a, { phase: 'start', block: 't1', kind: 'summary' });
    expect(start.blocks).toHaveLength(0);
    expect(a.blocks).toHaveLength(1);
    expect(b.blocks.map((x) => x.id)).toEqual(['t0', 't1']);
  });

  it('an update for an unseen block creates it (frames are best-effort)', () => {
    const turn = fold([{ phase: 'update', block: 't3', kind: 'raw', text: 'x' }]);
    expect(turn.blocks[0]).toMatchObject({ id: 't3', kind: 'raw', text: 'x', done: false });
  });

  it('a redacted frame clears the text and line already shown for that block, and it stays redacted', () => {
    const turn = fold([
      { phase: 'update', block: 't0', kind: 'summary', text: 'secret-ish', line: 'secret-ish' },
      { phase: 'update', block: 't0', kind: 'redacted' },
      { phase: 'end', block: 't0', kind: 'summary', text: 'late text' },
    ]);
    expect(turn.blocks[0].kind).toBe('redacted');
    expect(turn.blocks[0].text).toBeUndefined();
    expect(turn.blocks[0].line).toBeUndefined();
    expect(blocksWithText(turn)).toHaveLength(0);
    expect(latestThinkingLine(turn)).toBeUndefined();
  });

  it('omitted blocks never carry text; unknown kinds degrade to omitted', () => {
    const turn = fold([
      { phase: 'end', block: 't0', kind: 'omitted', text: 'should not show', tokens: 50 },
      { phase: 'end', block: 't1', kind: 'mystery', text: 'nor this' },
    ]);
    expect(turn.blocks.map((b) => b.kind)).toEqual(['omitted', 'omitted']);
    expect(turn.blocks.every((b) => b.text === undefined)).toBe(true);
    expect(thinkingTokens(turn)).toBe(50);
  });

  it('ignores unknown phases and frames without a block id, never throws', () => {
    const base = fold([{ phase: 'start', block: 't0', kind: 'summary' }]);
    expect(applyThinking(base, { phase: 'pause', block: 't0', kind: 'summary', text: 'x' })).toBe(base);
    expect(applyThinking(base, { phase: 'start', block: '', kind: 'summary' })).toBe(base);
    expect(() =>
      applyThinking(base, { phase: 'end', block: 't0', kind: 'summary', tokens: Number.NaN, text: 42 as never }),
    ).not.toThrow();
  });

  it('turn usage records whole-turn tokens without adding a display block', () => {
    const turn = fold([
      { phase: 'update', block: 't0', kind: 'summary', tokens: 100 },
      { phase: 'end', block: 'turn', kind: 'progress', tokens: 1240 },
    ]);
    expect(turn.blocks).toHaveLength(1);
    expect(turn.turnTokens).toBe(1240);
    expect(thinkingTokens(turn)).toBe(1240);
  });

  it('records raw_needs_redaction and truncation', () => {
    const turn = fold([
      { phase: 'end', block: 't0', kind: 'raw', raw_needs_redaction: true },
      { phase: 'end', block: 't1', kind: 'summary', text: 'long…', truncated: true },
    ]);
    expect(turn.rawNeedsRedaction).toBe(true);
    expect(turn.blocks[1].truncated).toBe(true);
  });
});

describe('turn timing and tokens', () => {
  it('seconds are wall time rounded up, never 0', () => {
    expect(thinkingSeconds(T0, T0)).toBe(1);
    expect(thinkingSeconds(T0, T0 + 400)).toBe(1);
    expect(thinkingSeconds(T0, T0 + 1000)).toBe(1);
    expect(thinkingSeconds(T0, T0 + 1001)).toBe(2);
    expect(thinkingSeconds(T0, T0 + 65_000)).toBe(65);
    expect(thinkingSeconds(T0, T0 - 50)).toBe(1);
  });

  it('finishTurnThinking stamps the arrival time and falls back to the done meta tokens', () => {
    const turn = finishTurnThinking(startTurnThinking(T0), T0 + 8200, { tokens: 900, blocks: 1, kinds: ['omitted'] });
    expect(turn.endedAt).toBe(T0 + 8200);
    expect(thinkingTokens(turn)).toBe(900);
    // Frame-reported usage wins over the meta.
    const withUsage = finishTurnThinking(fold([{ phase: 'end', block: 'turn', kind: 'progress', tokens: 50 }]), T0 + 1, {
      tokens: 900,
    });
    expect(thinkingTokens(withUsage)).toBe(50);
  });

  it('a turn with no frames still keeps its wall time and has no tokens', () => {
    const turn = finishTurnThinking(startTurnThinking(T0), T0 + 3500);
    expect(thinkingSeconds(turn.startedAt, turn.endedAt!)).toBe(4);
    expect(thinkingTokens(turn)).toBeUndefined();
  });

  it('formats approximate tokens', () => {
    expect(formatApproxTokens(840)).toBe('840');
    expect(formatApproxTokens(1000)).toBe('1k');
    expect(formatApproxTokens(1240)).toBe('1.2k');
    expect(formatApproxTokens(12_400)).toBe('12k');
  });

  it('validates the assistant_done thinking meta', () => {
    expect(parseThinkingDoneMeta(undefined)).toBeUndefined();
    expect(parseThinkingDoneMeta('x')).toBeUndefined();
    expect(parseThinkingDoneMeta({ tokens: 12, blocks: 2, kinds: ['summary', 3] })).toEqual({
      tokens: 12,
      blocks: 2,
      kinds: ['summary'],
    });
    expect(parseThinkingDoneMeta({ tokens: -1 })).toEqual({});
  });
});

describe('attribution filter covers thinking frames', () => {
  it('both frame-type lists include thinking', () => {
    expect(IN_TURN_FRAME_TYPES.has('thinking')).toBe(true);
    expect(CONVERSATION_FRAME_TYPES.has('thinking')).toBe(true);
    expect(CONVERSATION_FRAME_TYPES.has('assistant_done')).toBe(true);
  });

  it('drops a thinking frame from another conversation, keeps own and untagged ones', () => {
    expect(shouldRenderFrame('thinking', 'c-other', 'c-mine')).toBe(false);
    expect(shouldRenderFrame('thinking', 'c-mine', 'c-mine')).toBe(true);
    expect(shouldRenderFrame('thinking', undefined, 'c-mine')).toBe(true);
    // Connection-level frames always pass.
    expect(shouldRenderFrame('session_info', 'c-other', 'c-mine')).toBe(true);
    expect(shouldRenderFrame('error', 'c-other', 'c-mine')).toBe(true);
  });
});
