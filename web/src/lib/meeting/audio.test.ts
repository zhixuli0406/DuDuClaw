import { afterEach, describe, expect, it, vi } from 'vitest';
import { canExtractAudio, prepareMeetingAudio } from './audio';
const available = { AudioDecoder: {}, AudioEncoder: {}, AudioData: {}, EncodedAudioChunk: {} };
afterEach(() => vi.unstubAllGlobals());
describe('meeting audio fallback', () => {
  it('only attempts supported containers with all required WebCodecs APIs', () => {
    expect(canExtractAudio({ name: 'meeting.MOV' }, available)).toBe(true);
    expect(canExtractAudio({ name: 'meeting.mp4' }, {})).toBe(false);
    expect(canExtractAudio({ name: 'meeting.webm' }, available)).toBe(false);
    expect(canExtractAudio({ name: 'meeting.mp3' }, available)).toBe(false);
  });
  it('retains the original when APIs are missing', async () => {
    const file = new File(['recording'], 'meeting.mp4');
    const extract = vi.fn();
    expect(await prepareMeetingAudio(file, undefined, extract)).toEqual({ file, path: 'original' });
    expect(extract).not.toHaveBeenCalled();
  });
  it('falls back on every extraction error and on larger output', async () => {
    Object.entries(available).forEach(([key, value]) => vi.stubGlobal(key, value));
    const file = new File(['recording'], 'meeting.mp4');
    expect((await prepareMeetingAudio(file, undefined, async () => { throw new Error('unsupported codec'); })).file).toBe(file);
    expect((await prepareMeetingAudio(file, undefined, async () => new File(['much larger recording'], 'meeting.aac'))).file).toBe(file);
  });
  it('selects smaller audio and preserves abort instead of starting an original upload', async () => {
    Object.entries(available).forEach(([key, value]) => vi.stubGlobal(key, value));
    const file = new File(['recording'], 'meeting.mp4');
    const audio = new File(['a'], 'meeting.aac');
    expect(await prepareMeetingAudio(file, undefined, async () => audio)).toEqual({ file: audio, path: 'audio' });
    const abort = new AbortController(); abort.abort();
    await expect(prepareMeetingAudio(file, abort.signal, async () => { abort.signal.throwIfAborted(); return audio; })).rejects.toThrow();
  });
});
