/**
 * Pure state machine for push-to-talk voice capture (openhuman-parity B-P2).
 *
 * The React hook (`useVoiceRecorder`) owns the `MediaRecorder` + network side
 * effects; this reducer owns only the observable status so it can be unit
 * tested without a browser. Flow: idle → recording → transcribing →
 * idle (filled) | error.
 */

export type RecorderStatus = 'idle' | 'recording' | 'transcribing' | 'error';

export interface RecorderState {
  readonly status: RecorderStatus;
  /** Present only when `status === 'error'`. */
  readonly error?: string;
}

export type RecorderEvent =
  | { type: 'start' } //  idle → recording
  | { type: 'stop' } //   recording → transcribing (recorder stopped, upload begins)
  | { type: 'transcribed' } // transcribing → idle (text delivered)
  | { type: 'cancel' } // recording → idle (aborted before upload)
  | { type: 'error'; message: string }
  | { type: 'reset' }; // error → idle

export const INITIAL_RECORDER_STATE: RecorderState = { status: 'idle' };

/**
 * Advance the recorder state. Unknown transitions are no-ops (the machine never
 * throws — the UI wire is best-effort), so e.g. a duplicate `stop` is ignored.
 */
export function recorderReducer(state: RecorderState, ev: RecorderEvent): RecorderState {
  switch (ev.type) {
    case 'start':
      // Allow (re)start from idle or a prior error.
      return state.status === 'idle' || state.status === 'error'
        ? { status: 'recording' }
        : state;
    case 'stop':
      return state.status === 'recording' ? { status: 'transcribing' } : state;
    case 'cancel':
      return state.status === 'recording' ? { status: 'idle' } : state;
    case 'transcribed':
      return state.status === 'transcribing' ? { status: 'idle' } : state;
    case 'error':
      return { status: 'error', error: ev.message };
    case 'reset':
      return { status: 'idle' };
    default:
      return state;
  }
}

/** True while the mic is actively capturing (drives DuDu's `listening` face). */
export function isCapturing(state: RecorderState): boolean {
  return state.status === 'recording';
}

/** True while audio is uploading / being transcribed (drives a spinner). */
export function isTranscribing(state: RecorderState): boolean {
  return state.status === 'transcribing';
}
