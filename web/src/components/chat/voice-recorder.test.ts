import { describe, it, expect } from 'vitest';
import {
  recorderReducer,
  INITIAL_RECORDER_STATE,
  isCapturing,
  isTranscribing,
  type RecorderState,
} from './voice-recorder';

describe('recorderReducer', () => {
  it('walks the happy path idle → recording → transcribing → idle', () => {
    let s: RecorderState = INITIAL_RECORDER_STATE;
    expect(s.status).toBe('idle');

    s = recorderReducer(s, { type: 'start' });
    expect(s.status).toBe('recording');
    expect(isCapturing(s)).toBe(true);

    s = recorderReducer(s, { type: 'stop' });
    expect(s.status).toBe('transcribing');
    expect(isTranscribing(s)).toBe(true);

    s = recorderReducer(s, { type: 'transcribed' });
    expect(s.status).toBe('idle');
  });

  it('cancels a recording back to idle', () => {
    let s = recorderReducer(INITIAL_RECORDER_STATE, { type: 'start' });
    s = recorderReducer(s, { type: 'cancel' });
    expect(s.status).toBe('idle');
  });

  it('enters error from any state and recovers on reset/start', () => {
    let s = recorderReducer(INITIAL_RECORDER_STATE, { type: 'start' });
    s = recorderReducer(s, { type: 'error', message: 'mic denied' });
    expect(s.status).toBe('error');
    expect(s.error).toBe('mic denied');

    // start is allowed from error
    s = recorderReducer(s, { type: 'start' });
    expect(s.status).toBe('recording');

    s = recorderReducer(s, { type: 'error', message: 'network' });
    s = recorderReducer(s, { type: 'reset' });
    expect(s.status).toBe('idle');
    expect(s.error).toBeUndefined();
  });

  it('ignores illegal transitions (no throw, no state change)', () => {
    // stop while idle is a no-op
    expect(recorderReducer(INITIAL_RECORDER_STATE, { type: 'stop' }).status).toBe('idle');
    // transcribed while recording is a no-op
    const rec = recorderReducer(INITIAL_RECORDER_STATE, { type: 'start' });
    expect(recorderReducer(rec, { type: 'transcribed' }).status).toBe('recording');
    // start while already recording is a no-op
    expect(recorderReducer(rec, { type: 'start' }).status).toBe('recording');
  });
});
