import { useCallback, useEffect, useReducer, useRef } from 'react';
import { useChatStore } from '@/stores/chat-store';
import {
  recorderReducer,
  INITIAL_RECORDER_STATE,
  type RecorderState,
} from './voice-recorder';
import { sttTranscribe, VoiceNotConfiguredError } from './voice-api';

/** Whether this browser can capture microphone audio at all. */
export function micSupported(): boolean {
  return (
    typeof navigator !== 'undefined' &&
    !!navigator.mediaDevices?.getUserMedia &&
    typeof MediaRecorder !== 'undefined'
  );
}

/** Pick the best available recording container. Empty string → let the UA choose. */
function pickMimeType(): string {
  if (typeof MediaRecorder === 'undefined' || !MediaRecorder.isTypeSupported) return '';
  const candidates = ['audio/webm;codecs=opus', 'audio/webm', 'audio/ogg;codecs=opus', 'audio/mp4'];
  return candidates.find((c) => MediaRecorder.isTypeSupported(c)) ?? '';
}

function extForMime(mime: string): string {
  if (mime.includes('ogg')) return 'ogg';
  if (mime.includes('mp4')) return 'm4a';
  return 'webm';
}

export interface VoiceRecorderHandle {
  readonly state: RecorderState;
  readonly supported: boolean;
  /** Begin capture (requests mic permission on first use). */
  start: () => Promise<void>;
  /** Stop capture → upload → transcribe → `onTranscript`. */
  stop: () => void;
}

/**
 * Push-to-talk recorder. Owns the `MediaRecorder` + `/api/stt` upload; mirrors
 * capturing/transcribing into the chat store so DuDu shows the `listening`
 * face. On success calls `onTranscript(text)` — the caller fills the composer
 * (we never auto-send; the human confirms).
 */
export function useVoiceRecorder(opts: {
  onTranscript: (text: string) => void;
  onNotConfigured?: (message: string) => void;
  onError?: (message: string) => void;
}): VoiceRecorderHandle {
  const [state, dispatch] = useReducer(recorderReducer, INITIAL_RECORDER_STATE);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const setRecording = useChatStore((s) => s.setRecording);
  const setTranscribing = useChatStore((s) => s.setTranscribing);

  // Keep the latest callbacks without re-creating start/stop each render.
  const cbRef = useRef(opts);
  cbRef.current = opts;

  const supported = micSupported();

  const cleanupStream = useCallback(() => {
    streamRef.current?.getTracks().forEach((t) => t.stop());
    streamRef.current = null;
  }, []);

  const start = useCallback(async () => {
    if (!supported) {
      dispatch({ type: 'error', message: 'unsupported' });
      cbRef.current.onError?.('unsupported');
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      streamRef.current = stream;
      const mime = pickMimeType();
      const mr = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream);
      chunksRef.current = [];

      mr.ondataavailable = (e) => {
        if (e.data && e.data.size > 0) chunksRef.current.push(e.data);
      };
      mr.onstop = async () => {
        cleanupStream();
        const type = mr.mimeType || 'audio/webm';
        const blob = new Blob(chunksRef.current, { type });
        chunksRef.current = [];
        dispatch({ type: 'stop' });
        setRecording(false);
        setTranscribing(true);
        try {
          const text = await sttTranscribe(blob, `voice.${extForMime(type)}`);
          dispatch({ type: 'transcribed' });
          if (text.trim()) cbRef.current.onTranscript(text.trim());
        } catch (err) {
          if (err instanceof VoiceNotConfiguredError) {
            dispatch({ type: 'reset' });
            cbRef.current.onNotConfigured?.(err.message);
          } else {
            const msg = err instanceof Error ? err.message : String(err);
            dispatch({ type: 'error', message: msg });
            cbRef.current.onError?.(msg);
          }
        } finally {
          setTranscribing(false);
        }
      };

      recorderRef.current = mr;
      mr.start();
      dispatch({ type: 'start' });
      setRecording(true);
    } catch (err) {
      // getUserMedia rejects on permission denial / no device.
      cleanupStream();
      setRecording(false);
      const msg = err instanceof Error ? err.message : String(err);
      dispatch({ type: 'error', message: msg });
      cbRef.current.onError?.(msg);
    }
  }, [supported, cleanupStream, setRecording, setTranscribing]);

  const stop = useCallback(() => {
    const mr = recorderRef.current;
    if (mr && mr.state !== 'inactive') {
      mr.stop(); // fires onstop → upload
    }
  }, []);

  // Safety: stop capture + release the mic if the component unmounts mid-record.
  useEffect(() => {
    return () => {
      const mr = recorderRef.current;
      if (mr && mr.state !== 'inactive') {
        try {
          mr.stop();
        } catch {
          /* ignore */
        }
      }
      cleanupStream();
      setRecording(false);
    };
  }, [cleanupStream, setRecording]);

  return { state, supported, start, stop };
}
