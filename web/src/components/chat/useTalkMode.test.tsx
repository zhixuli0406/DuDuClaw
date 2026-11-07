import { act, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useChatStore } from '@/stores/chat-store';
import { useTalkMode } from './useTalkMode';
import { sttTranscribe } from './voice-api';

vi.mock('./voice-api', () => ({
  sttPreflight: vi.fn().mockResolvedValue(undefined),
  sttTranscribe: vi.fn(),
  ttsSynthesizeUrl: vi.fn(),
  VoiceNotConfiguredError: class extends Error {},
}));

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it('re-checks send after STT resolves and recovers when rejected even with an existing streaming turn', async () => {
  vi.useFakeTimers();
  let transcribed!: (text: string) => void;
  vi.mocked(sttTranscribe).mockReturnValue(new Promise(resolve => { transcribed = resolve; }));
  let voiced = true;
  let now = 0;
  vi.spyOn(performance, 'now').mockImplementation(() => now);
  const track = { enabled: true, stop: vi.fn() };
  Object.defineProperty(navigator, 'mediaDevices', { configurable: true, value: {
    getUserMedia: vi.fn().mockResolvedValue({ getTracks: () => [track], getAudioTracks: () => [track] }),
  } });
  vi.stubGlobal('MediaRecorder', class {
    static isTypeSupported() { return true; }
    state = 'inactive';
    mimeType = 'audio/webm';
    onstop: (() => void) | null = null;
    start() { this.state = 'recording'; }
    stop() { this.state = 'inactive'; this.onstop?.(); }
  });
  vi.stubGlobal('AudioContext', class {
    createMediaStreamSource() { return { connect: vi.fn() }; }
    createAnalyser() { return { fftSize: 2048, getFloatTimeDomainData: (buffer: Float32Array) => buffer.fill(voiced ? 0.1 : 0) }; }
    close() { return Promise.resolve(); }
  });
  const originalSend = useChatStore.getState().send;
  const send = vi.fn().mockReturnValue(true);
  useChatStore.setState({ send, isStreaming: true, phase: 'thinking' });
  const onError = vi.fn();
  const { result, unmount } = renderHook(() => useTalkMode({ onError }));
  try {
    await act(async () => { result.current.toggle(); });
    for (const time of [50, 450]) {
      now = time;
      await act(async () => { await vi.advanceTimersByTimeAsync(50); });
    }
    voiced = false;
    now = 1700;
    await act(async () => { await vi.advanceTimersByTimeAsync(50); });
    expect(sttTranscribe).toHaveBeenCalledTimes(1);
    expect(send).not.toHaveBeenCalled();
    // Permissions have changed while transcription was in flight.
    send.mockReturnValue(false);
    await act(async () => { transcribed('voice message'); });
    expect(send).toHaveBeenCalledWith('voice message');
    expect(onError).toHaveBeenCalledWith('disconnected');
    expect(result.current.status).toBe('listening');
  } finally {
    unmount();
    useChatStore.setState({ send: originalSend, isStreaming: false });
    Reflect.deleteProperty(navigator, 'mediaDevices');
  }
});
