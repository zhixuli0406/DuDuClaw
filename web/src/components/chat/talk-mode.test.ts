import { describe, it, expect } from 'vitest';
import {
  DEFAULT_VAD_CONFIG,
  INITIAL_TALK_STATE,
  INITIAL_VAD_STATE,
  isTalkActive,
  rmsOf,
  segmentRmsSeries,
  talkReducer,
  vadStep,
  type TalkState,
  type VadConfig,
} from './talk-mode';

// ── Loop state machine ──────────────────────────────────────────

describe('talkReducer', () => {
  it('walks the full conversation loop and back to listening', () => {
    let s: TalkState = INITIAL_TALK_STATE;
    expect(s.status).toBe('idle');
    expect(isTalkActive(s)).toBe(false);

    s = talkReducer(s, { type: 'engage' });
    expect(s.status).toBe('listening');
    expect(isTalkActive(s)).toBe(true);

    s = talkReducer(s, { type: 'segment' });
    expect(s.status).toBe('transcribing');

    s = talkReducer(s, { type: 'transcript', text: '你好' });
    expect(s.status).toBe('awaiting-reply');

    s = talkReducer(s, { type: 'reply-done' });
    expect(s.status).toBe('speaking');

    s = talkReducer(s, { type: 'speech-ended' });
    expect(s.status).toBe('listening'); // loop continues
  });

  it('routes an empty transcript back to listening (noise, nothing sent)', () => {
    let s = talkReducer(INITIAL_TALK_STATE, { type: 'engage' });
    s = talkReducer(s, { type: 'segment' });
    s = talkReducer(s, { type: 'transcript', text: '   ' });
    expect(s.status).toBe('listening');
  });

  it('recover drops every busy state back to listening, never sticks', () => {
    for (const busy of ['listening', 'transcribing', 'awaiting-reply', 'speaking'] as const) {
      const s = talkReducer({ status: busy }, { type: 'recover' });
      expect(s.status).toBe('listening');
    }
    // recover while off stays off (a late error callback must not re-engage).
    expect(talkReducer(INITIAL_TALK_STATE, { type: 'recover' }).status).toBe('idle');
  });

  it('disengage exits from any state', () => {
    for (const st of ['idle', 'listening', 'transcribing', 'awaiting-reply', 'speaking'] as const) {
      expect(talkReducer({ status: st }, { type: 'disengage' }).status).toBe('idle');
    }
  });

  it('ignores illegal transitions (stale async events are no-ops)', () => {
    // segment while idle — mode already off when the VAD tick landed.
    expect(talkReducer(INITIAL_TALK_STATE, { type: 'segment' }).status).toBe('idle');
    // transcript while listening — duplicate STT resolution.
    expect(
      talkReducer({ status: 'listening' }, { type: 'transcript', text: 'x' }).status,
    ).toBe('listening');
    // reply-done while speaking — duplicate phase event.
    expect(talkReducer({ status: 'speaking' }, { type: 'reply-done' }).status).toBe('speaking');
    // speech-ended while transcribing — orphan audio event.
    expect(talkReducer({ status: 'transcribing' }, { type: 'speech-ended' }).status).toBe(
      'transcribing',
    );
    // engage while already on.
    expect(talkReducer({ status: 'listening' }, { type: 'engage' }).status).toBe('listening');
  });
});

// ── VAD-lite silence segmentation ───────────────────────────────

// Test config with round numbers: 1200ms silence, 300ms min speech.
const CFG: VadConfig = {
  threshold: 0.015,
  silenceMs: 1200,
  minSpeechMs: 300,
  maxSegmentMs: 60_000,
};
const TICK = 100; // ms between samples in these tests

/** n samples of a constant RMS level. */
function level(n: number, rms: number): number[] {
  return Array.from({ length: n }, () => rms);
}

describe('vadStep / segmentRmsSeries', () => {
  it('stays silent on sub-threshold noise (no events)', () => {
    const events = segmentRmsSeries(level(100, 0.005), TICK, CFG);
    expect(events).toEqual([]);
  });

  it('emits speech-start then a segment after sustained silence', () => {
    // 1s of speech, then 1.5s of silence.
    const series = [...level(10, 0.05), ...level(15, 0.001)];
    const events = segmentRmsSeries(series, TICK, CFG);
    expect(events.map((e) => e.outcome)).toEqual(['speech-start', 'segment']);
    // Speech starts at sample 0; segment closes once silence ≥ 1200ms after
    // the last voiced sample (sample 9 @ 900ms → closes at 2100ms).
    expect(events[0].atMs).toBe(0);
    expect(events[1].atMs).toBe(2100);
  });

  it('does not close a segment during a short mid-sentence pause', () => {
    // speech(1s) → pause(0.8s < silenceMs) → speech(1s) → silence(1.5s)
    const series = [
      ...level(10, 0.05),
      ...level(8, 0.001),
      ...level(10, 0.05),
      ...level(15, 0.001),
    ];
    const events = segmentRmsSeries(series, TICK, CFG);
    // One speech-start, one segment — the pause did not split it.
    expect(events.map((e) => e.outcome)).toEqual(['speech-start', 'segment']);
  });

  it('discards voiced blips shorter than minSpeechMs (breathing/noise)', () => {
    // 200ms of voice (< 300ms minimum), then silence.
    const series = [...level(2, 0.05), ...level(15, 0.001)];
    const events = segmentRmsSeries(series, TICK, CFG);
    expect(events.map((e) => e.outcome)).toEqual(['speech-start', 'discard']);
  });

  it('counts a voiced span exactly at minSpeechMs as a segment', () => {
    // Voiced samples at 0..300ms inclusive → span 300ms == minSpeechMs.
    const series = [...level(4, 0.05), ...level(15, 0.001)];
    const events = segmentRmsSeries(series, TICK, CFG);
    expect(events.map((e) => e.outcome)).toEqual(['speech-start', 'segment']);
  });

  it('force-ends a runaway segment at maxSegmentMs even while still voiced', () => {
    const cfg: VadConfig = { ...CFG, maxSegmentMs: 1000 };
    const series = level(30, 0.05); // 3s of continuous speech
    const events = segmentRmsSeries(series, TICK, cfg);
    expect(events[0]).toEqual({ atMs: 0, outcome: 'speech-start' });
    expect(events[1]).toEqual({ atMs: 1000, outcome: 'segment' });
    // Detector resets and starts a fresh segment right after.
    expect(events[2].outcome).toBe('speech-start');
  });

  it('vadStep is pure — same inputs, same outputs, no input mutation', () => {
    const s = { ...INITIAL_VAD_STATE };
    const a = vadStep(s, 0.05, 1000, CFG);
    const b = vadStep(s, 0.05, 1000, CFG);
    expect(a).toEqual(b);
    expect(s).toEqual(INITIAL_VAD_STATE);
    expect(a.next).toEqual({ speaking: true, speechStartMs: 1000, lastVoiceMs: 1000 });
  });

  it('uses the default config when none is given', () => {
    const { next, outcome } = vadStep(INITIAL_VAD_STATE, DEFAULT_VAD_CONFIG.threshold, 0);
    expect(outcome).toBe('speech-start');
    expect(next.speaking).toBe(true);
  });
});

describe('rmsOf', () => {
  it('is 0 for an empty or all-zero buffer', () => {
    expect(rmsOf([])).toBe(0);
    expect(rmsOf(new Float32Array(8))).toBe(0);
  });

  it('computes the root mean square of a known signal', () => {
    // Constant amplitude 0.5 → RMS 0.5; alternating ±0.5 likewise.
    expect(rmsOf([0.5, 0.5, 0.5, 0.5])).toBeCloseTo(0.5, 10);
    expect(rmsOf([0.5, -0.5, 0.5, -0.5])).toBeCloseTo(0.5, 10);
    // Half zeros halves the power: RMS = sqrt(0.25/2).
    expect(rmsOf([0.5, 0, 0.5, 0])).toBeCloseTo(Math.sqrt(0.125), 10);
  });
});
