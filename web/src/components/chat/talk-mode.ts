/**
 * Pure logic for Talk Mode (G13) — continuous voice conversation loop.
 *
 * Two independent, browser-free pieces so both are unit-testable:
 *
 *  1. The loop state machine (`talkReducer`): idle → listening → transcribing
 *     → awaiting-reply → speaking → listening → … Unknown transitions are
 *     no-ops (same convention as `recorderReducer`) so a stray async event can
 *     never corrupt or wedge the machine.
 *
 *  2. VAD-lite silence segmentation (`vadStep`): a step function fed one RMS
 *     sample at a time. Speech starts when RMS crosses `threshold`; after
 *     `silenceMs` of sustained silence the segment ends. Segments whose voiced
 *     span is shorter than `minSpeechMs` (breathing, keyboard noise) are
 *     discarded, not submitted. A running segment longer than `maxSegmentMs`
 *     is force-ended so a monologue can't grow unbounded.
 *
 * The React hook (`useTalkMode`) owns the mic / WebAudio / network side
 * effects and drives both machines.
 */

// ── Tunables (v1: fixed constants, no auto-calibration) ─────────

/** Sustained silence (ms) after speech that closes a segment. */
export const TALK_SILENCE_MS = 1200;
/** Voiced spans shorter than this (ms) are discarded as noise. */
export const TALK_MIN_SPEECH_MS = 300;
/** RMS level above which a sample counts as speech (not auto-calibrated in v1). */
export const TALK_SPEECH_RMS_THRESHOLD = 0.015;
/** Hard cap on one voiced segment — force-submit past this. */
export const TALK_MAX_SEGMENT_MS = 60_000;
/** How often the analyser is sampled while listening. */
export const TALK_SAMPLE_INTERVAL_MS = 50;
/** With no speech at all for this long, the recorder is restarted so the
 *  buffered blob of pure silence doesn't grow unbounded. */
export const TALK_QUIET_RESTART_MS = 20_000;

// ── Loop state machine ──────────────────────────────────────────

export type TalkStatus =
  | 'idle' //           mode off
  | 'listening' //      mic open, waiting for a voiced segment
  | 'transcribing' //   segment captured, STT in flight
  | 'awaiting-reply' // transcript sent as a chat message, agent replying
  | 'speaking'; //      agent reply playing via TTS (mic paused — no barge-in)

export interface TalkState {
  readonly status: TalkStatus;
}

export type TalkTransitionEvent =
  | { type: 'engage' } //          idle → listening (mode turned on)
  | { type: 'segment' } //         listening → transcribing (silence closed a segment)
  | { type: 'transcript'; text: string } // transcribing → awaiting-reply (non-empty) | listening (empty)
  | { type: 'reply-done' } //      awaiting-reply → speaking (reply finished, TTS starts)
  | { type: 'speech-ended' } //    speaking → listening (playback finished / skipped)
  | { type: 'recover' } //         transcribing | awaiting-reply | speaking → listening (error recovery)
  | { type: 'disengage' }; //      any → idle (toggle off / Esc / unmount)

export const INITIAL_TALK_STATE: TalkState = { status: 'idle' };

/**
 * Advance the talk-mode loop. Pure; unknown transitions return the input state
 * unchanged so duplicate/late async events are harmless. `recover` exists so
 * every failure path (STT error, send failure, TTS error) lands back in
 * `listening` — the loop never sticks in a busy state.
 */
export function talkReducer(state: TalkState, ev: TalkTransitionEvent): TalkState {
  switch (ev.type) {
    case 'engage':
      return state.status === 'idle' ? { status: 'listening' } : state;
    case 'segment':
      return state.status === 'listening' ? { status: 'transcribing' } : state;
    case 'transcript':
      if (state.status !== 'transcribing') return state;
      return ev.text.trim() ? { status: 'awaiting-reply' } : { status: 'listening' };
    case 'reply-done':
      return state.status === 'awaiting-reply' ? { status: 'speaking' } : state;
    case 'speech-ended':
      return state.status === 'speaking' ? { status: 'listening' } : state;
    case 'recover':
      // Any busy state drops back to listening; idle stays idle (mode off).
      return state.status === 'idle' ? state : { status: 'listening' };
    case 'disengage':
      return { status: 'idle' };
    default:
      return state;
  }
}

/** True whenever the mode is on (any non-idle status). */
export function isTalkActive(state: TalkState): boolean {
  return state.status !== 'idle';
}

// ── VAD-lite silence segmentation ───────────────────────────────

export interface VadConfig {
  /** RMS above which a sample counts as voiced. */
  readonly threshold: number;
  /** Sustained silence (ms) after speech that ends the segment. */
  readonly silenceMs: number;
  /** Voiced spans shorter than this (ms) are discarded, not submitted. */
  readonly minSpeechMs: number;
  /** A segment voiced longer than this (ms) is force-ended. */
  readonly maxSegmentMs: number;
}

export const DEFAULT_VAD_CONFIG: VadConfig = {
  threshold: TALK_SPEECH_RMS_THRESHOLD,
  silenceMs: TALK_SILENCE_MS,
  minSpeechMs: TALK_MIN_SPEECH_MS,
  maxSegmentMs: TALK_MAX_SEGMENT_MS,
};

export interface VadState {
  /** True once speech has been detected in the current segment. */
  readonly speaking: boolean;
  /** Timestamp (ms) of the first voiced sample of the segment. */
  readonly speechStartMs: number;
  /** Timestamp (ms) of the most recent voiced sample. */
  readonly lastVoiceMs: number;
}

export const INITIAL_VAD_STATE: VadState = {
  speaking: false,
  speechStartMs: 0,
  lastVoiceMs: 0,
};

export type VadOutcome =
  | 'none' //         nothing to do
  | 'speech-start' // first voiced sample of a segment
  | 'segment' //      a valid segment just ended → submit the recording
  | 'discard'; //     a too-short voiced blip ended → drop the recording

/**
 * Feed one RMS sample into the detector. Pure: returns the next state plus
 * what (if anything) the caller should do. `nowMs` is any monotonic clock.
 */
export function vadStep(
  state: VadState,
  rms: number,
  nowMs: number,
  cfg: VadConfig = DEFAULT_VAD_CONFIG,
): { next: VadState; outcome: VadOutcome } {
  const voiced = rms >= cfg.threshold;

  if (!state.speaking) {
    if (!voiced) return { next: state, outcome: 'none' };
    return {
      next: { speaking: true, speechStartMs: nowMs, lastVoiceMs: nowMs },
      outcome: 'speech-start',
    };
  }

  // Force-end a runaway segment regardless of current voicing.
  if (nowMs - state.speechStartMs >= cfg.maxSegmentMs) {
    return { next: INITIAL_VAD_STATE, outcome: 'segment' };
  }

  if (voiced) {
    return { next: { ...state, lastVoiceMs: nowMs }, outcome: 'none' };
  }

  // Silent while a segment is open: close it after sustained silence.
  if (nowMs - state.lastVoiceMs >= cfg.silenceMs) {
    const voicedSpanMs = state.lastVoiceMs - state.speechStartMs;
    return {
      next: INITIAL_VAD_STATE,
      outcome: voicedSpanMs >= cfg.minSpeechMs ? 'segment' : 'discard',
    };
  }
  return { next: state, outcome: 'none' };
}

/** Root-mean-square of a time-domain sample buffer (AnalyserNode float data). */
export function rmsOf(samples: ArrayLike<number>): number {
  const n = samples.length;
  if (n === 0) return 0;
  let sum = 0;
  for (let i = 0; i < n; i += 1) {
    const v = samples[i];
    sum += v * v;
  }
  return Math.sqrt(sum / n);
}

/**
 * Run a whole RMS series through the detector (test/analysis convenience).
 * Sample `i` is taken at `i * intervalMs`. Returns every non-`none` outcome
 * with its timestamp.
 */
export function segmentRmsSeries(
  series: readonly number[],
  intervalMs: number,
  cfg: VadConfig = DEFAULT_VAD_CONFIG,
): Array<{ atMs: number; outcome: Exclude<VadOutcome, 'none'> }> {
  const events: Array<{ atMs: number; outcome: Exclude<VadOutcome, 'none'> }> = [];
  let state = INITIAL_VAD_STATE;
  for (let i = 0; i < series.length; i += 1) {
    const atMs = i * intervalMs;
    const { next, outcome } = vadStep(state, series[i], atMs, cfg);
    state = next;
    if (outcome !== 'none') events.push({ atMs, outcome });
  }
  return events;
}
