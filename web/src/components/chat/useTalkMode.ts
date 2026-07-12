import { useCallback, useEffect, useRef, useState } from 'react';
import { useChatStore } from '@/stores/chat-store';
import {
  INITIAL_VAD_STATE,
  TALK_QUIET_RESTART_MS,
  TALK_SAMPLE_INTERVAL_MS,
  rmsOf,
  talkReducer,
  vadStep,
  type TalkStatus,
  type TalkTransitionEvent,
  type VadState,
} from './talk-mode';
import { extForMime, micSupported, pickMimeType } from './useVoiceRecorder';
import { sttPreflight, sttTranscribe, ttsSynthesizeUrl, VoiceNotConfiguredError } from './voice-api';

/**
 * Talk Mode (G13) — continuous voice conversation loop for the dashboard chat.
 *
 * Loop: listen (mic open, WebAudio RMS VAD) → silence ends a voiced segment →
 * segment auto-submitted to `POST /api/stt` → transcript sent through the
 * normal chat `send()` path (it appears as a regular user message) → the
 * agent's finished reply is spoken via `POST /api/tts` → mic resumes. Toggle
 * off (button or Esc) exits from any point: in-flight fetches are aborted,
 * playback stops, and the `MediaStream` tracks are released.
 *
 * Pure logic (state machine + VAD math) lives in `talk-mode.ts`; this hook
 * owns only the browser/network side effects. Fail-closed: engaging first
 * probes STT via {@link sttPreflight} and refuses to start (with the existing
 * friendly guidance) when STT is unconfigured. Recoverable mid-loop errors
 * (STT 4xx/5xx, TTS failure, send failure) surface through the `onError` /
 * `onTtsFailed` callbacks and drop back to `listening` — never a stuck state.
 *
 * Honest v1 limits:
 * - No wake word — the mode is engaged by the dashboard toggle only.
 * - No barge-in — while the agent reply is playing the mic is PAUSED
 *   (tracks disabled, recorder stopped); the user cannot interrupt by voice.
 * - Browser-only — requires `getUserMedia` mic permission + `MediaRecorder`.
 * - The silence/speech RMS threshold is a fixed constant, not auto-calibrated
 *   to ambient noise (see `TALK_SPEECH_RMS_THRESHOLD`).
 */
export interface TalkModeHandle {
  /** Current loop status (drives the composer indicator). */
  readonly status: TalkStatus;
  /** True whenever the mode is on (any non-idle status). */
  readonly active: boolean;
  /** Whether this browser can capture microphone audio at all. */
  readonly supported: boolean;
  /** Engage when idle; disengage otherwise. */
  toggle: () => void;
  /** Force off from any state (Esc / unmount path). */
  stop: () => void;
}

export function useTalkMode(opts: {
  /** STT (or TTS at engage time) is unconfigured — show the friendly guidance. */
  onNotConfigured?: (message: string) => void;
  /** Recoverable loop error (STT failure, send failure); mode stays on. */
  onError?: (message: string) => void;
  /** TTS synthesis/playback failed; the reply text is still in the thread. */
  onTtsFailed?: (message: string) => void;
  /** Engaging failed outright (mic denied, gateway unreachable). */
  onEngageFailed?: (message: string) => void;
}): TalkModeHandle {
  const [status, setStatus] = useState<TalkStatus>('idle');
  const statusRef = useRef<TalkStatus>('idle');

  // Generation token: every engage/disengage bumps it; async continuations
  // check it and quietly exit when stale — this is what makes toggling off
  // mid-cycle cancellation-safe.
  const genRef = useRef(0);

  const streamRef = useRef<MediaStream | null>(null);
  const audioCtxRef = useRef<AudioContext | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const sampleBufRef = useRef<Float32Array<ArrayBuffer> | null>(null);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const recorderStartedAtRef = useRef(0);
  const vadRef = useRef<VadState>(INITIAL_VAD_STATE);
  const samplerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const objectUrlRef = useRef<string | null>(null);
  const spokenIdRef = useRef<string | null>(null);

  const setRecording = useChatStore((s) => s.setRecording);
  const setTranscribing = useChatStore((s) => s.setTranscribing);

  // Latest callbacks without re-creating the loop functions each render.
  const cbRef = useRef(opts);
  cbRef.current = opts;

  const supported = micSupported();

  /** Synchronous transition through the pure reducer (ref mirror + render state). */
  const transition = useCallback((ev: TalkTransitionEvent): TalkStatus => {
    const next = talkReducer({ status: statusRef.current }, ev);
    statusRef.current = next.status;
    setStatus(next.status);
    return next.status;
  }, []);

  /** Enable/disable the mic tracks (pause vs live) without releasing them. */
  const setMicEnabled = useCallback((enabled: boolean) => {
    streamRef.current?.getAudioTracks().forEach((t) => {
      t.enabled = enabled;
    });
  }, []);

  /** Stop + drop the current recorder without firing our onstop pipeline. */
  const stopRecorderSilently = useCallback(() => {
    const mr = recorderRef.current;
    recorderRef.current = null;
    if (mr && mr.state !== 'inactive') {
      mr.onstop = null;
      mr.ondataavailable = null;
      try {
        mr.stop();
      } catch {
        /* already stopped */
      }
    }
    chunksRef.current = [];
  }, []);

  /** Start a fresh recorder segment on the live stream. */
  const startRecorder = useCallback(() => {
    stopRecorderSilently();
    const stream = streamRef.current;
    if (!stream) return;
    const mime = pickMimeType();
    const mr = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream);
    mr.ondataavailable = (e) => {
      if (e.data && e.data.size > 0) chunksRef.current.push(e.data);
    };
    recorderRef.current = mr;
    recorderStartedAtRef.current = performance.now();
    vadRef.current = INITIAL_VAD_STATE;
    mr.start();
  }, [stopRecorderSilently]);

  /** Side effects of (re)entering `listening`: mic live + fresh recorder. */
  const resumeListeningEffects = useCallback(
    (gen: number) => {
      if (gen !== genRef.current || statusRef.current !== 'listening') return;
      setTranscribing(false);
      setMicEnabled(true);
      startRecorder();
      setRecording(true);
    },
    [setMicEnabled, startRecorder, setRecording, setTranscribing],
  );

  /** Any busy-state failure → toast handled by caller, loop back to listening. */
  const recover = useCallback(
    (gen: number) => {
      if (gen !== genRef.current) return;
      transition({ type: 'recover' });
      resumeListeningEffects(gen);
    },
    [transition, resumeListeningEffects],
  );

  /** Full teardown: abort fetches, stop audio, release the mic. */
  const disengage = useCallback(() => {
    genRef.current += 1;
    abortRef.current?.abort();
    abortRef.current = null;

    const audio = audioRef.current;
    audioRef.current = null;
    if (audio) {
      try {
        audio.pause();
      } catch {
        /* ignore */
      }
    }
    if (objectUrlRef.current) {
      URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
    }

    if (samplerRef.current) {
      clearInterval(samplerRef.current);
      samplerRef.current = null;
    }
    stopRecorderSilently();
    streamRef.current?.getTracks().forEach((t) => t.stop());
    streamRef.current = null;
    void audioCtxRef.current?.close().catch(() => {});
    audioCtxRef.current = null;
    analyserRef.current = null;
    sampleBufRef.current = null;
    vadRef.current = INITIAL_VAD_STATE;
    spokenIdRef.current = null;

    setRecording(false);
    setTranscribing(false);
    transition({ type: 'disengage' });
  }, [stopRecorderSilently, setRecording, setTranscribing, transition]);

  /** Speak a finished reply, then resume listening. */
  const speakReply = useCallback(
    async (gen: number, text: string) => {
      if (gen !== genRef.current) return;
      transition({ type: 'reply-done' }); // awaiting-reply → speaking
      let objectUrl: string | null = null;
      try {
        objectUrl = await ttsSynthesizeUrl(text, '', abortRef.current?.signal);
        if (gen !== genRef.current) {
          URL.revokeObjectURL(objectUrl);
          return;
        }
        const audio = new Audio(objectUrl);
        audioRef.current = audio;
        objectUrlRef.current = objectUrl;
        await new Promise<void>((resolve) => {
          audio.onended = () => resolve();
          audio.onerror = () => resolve();
          // Autoplay policies can reject; the toggle click is a user gesture so
          // this normally succeeds — on rejection we just skip the speech.
          audio.play().catch(() => resolve());
        });
        if (audioRef.current === audio) audioRef.current = null;
        if (objectUrlRef.current === objectUrl) {
          URL.revokeObjectURL(objectUrl);
          objectUrlRef.current = null;
        }
        if (gen !== genRef.current) return;
        transition({ type: 'speech-ended' });
        resumeListeningEffects(gen);
      } catch (err) {
        if (objectUrl && objectUrlRef.current !== objectUrl) URL.revokeObjectURL(objectUrl);
        if (gen !== genRef.current) return; // aborted by disengage
        const msg = err instanceof Error ? err.message : String(err);
        cbRef.current.onTtsFailed?.(msg);
        recover(gen);
      }
    },
    [transition, resumeListeningEffects, recover],
  );

  /** Close the current voiced segment: stop recorder → STT → chat send. */
  const endSegment = useCallback(
    async (gen: number) => {
      const mr = recorderRef.current;
      if (!mr || mr.state === 'inactive' || gen !== genRef.current) return;
      recorderRef.current = null;

      transition({ type: 'segment' }); // listening → transcribing
      setRecording(false);
      setTranscribing(true);
      setMicEnabled(false); // mic paused until we're back to listening

      const blob = await new Promise<Blob>((resolve) => {
        mr.onstop = () => {
          const type = mr.mimeType || 'audio/webm';
          const b = new Blob(chunksRef.current, { type });
          chunksRef.current = [];
          resolve(b);
        };
        try {
          mr.stop();
        } catch {
          resolve(new Blob(chunksRef.current, { type: 'audio/webm' }));
          chunksRef.current = [];
        }
      });
      if (gen !== genRef.current) return;

      try {
        const text = (
          await sttTranscribe(blob, `voice.${extForMime(blob.type)}`, abortRef.current?.signal)
        ).trim();
        if (gen !== genRef.current) return;
        setTranscribing(false);
        transition({ type: 'transcript', text }); // → awaiting-reply | listening
        if (!text) {
          // Nothing recognized (noise) — quietly resume listening.
          resumeListeningEffects(gen);
          return;
        }
        const store = useChatStore.getState();
        store.send(text); // normal user message through the normal send path
        if (!useChatStore.getState().isStreaming) {
          // send() no-ops when the socket is down — don't wait for a reply
          // that will never come.
          cbRef.current.onError?.('disconnected');
          recover(gen);
        }
        // Otherwise: awaiting-reply — the phase watcher takes over.
      } catch (err) {
        if (gen !== genRef.current) return; // aborted by disengage
        setTranscribing(false);
        if (err instanceof VoiceNotConfiguredError) {
          // STT got unconfigured mid-session — fail closed and exit the mode.
          cbRef.current.onNotConfigured?.(err.message);
          disengage();
          return;
        }
        const msg = err instanceof Error ? err.message : String(err);
        cbRef.current.onError?.(msg);
        recover(gen);
      }
    },
    [transition, setRecording, setTranscribing, setMicEnabled, resumeListeningEffects, recover, disengage],
  );

  /** One VAD sample while listening. */
  const samplerTick = useCallback(() => {
    if (statusRef.current !== 'listening') return;
    const analyser = analyserRef.current;
    const buf = sampleBufRef.current;
    if (!analyser || !buf) return;
    analyser.getFloatTimeDomainData(buf);
    const now = performance.now();
    const { next, outcome } = vadStep(vadRef.current, rmsOf(buf), now);
    vadRef.current = next;
    if (outcome === 'segment') {
      void endSegment(genRef.current);
    } else if (outcome === 'discard') {
      // Sub-minimum voiced blip (breath/noise): drop it, keep listening.
      startRecorder();
    } else if (
      !next.speaking &&
      now - recorderStartedAtRef.current >= TALK_QUIET_RESTART_MS
    ) {
      // Long stretch of pure silence: restart the recorder so the buffered
      // blob doesn't grow unbounded.
      startRecorder();
    }
  }, [endSegment, startRecorder]);

  const engage = useCallback(async () => {
    if (statusRef.current !== 'idle') return;
    if (!supported) {
      cbRef.current.onEngageFailed?.('unsupported');
      return;
    }
    const gen = ++genRef.current;
    const ac = new AbortController();
    abortRef.current = ac;

    // Fail closed: refuse to open the mic when STT is unconfigured.
    try {
      await sttPreflight(ac.signal);
    } catch (err) {
      if (gen !== genRef.current) return;
      if (err instanceof VoiceNotConfiguredError) {
        cbRef.current.onNotConfigured?.(err.message);
      } else {
        cbRef.current.onEngageFailed?.(err instanceof Error ? err.message : String(err));
      }
      return;
    }

    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      if (gen !== genRef.current) {
        stream.getTracks().forEach((t) => t.stop());
        return;
      }
      streamRef.current = stream;
      const ctx = new AudioContext();
      audioCtxRef.current = ctx;
      const source = ctx.createMediaStreamSource(stream);
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 2048;
      source.connect(analyser);
      analyserRef.current = analyser;
      sampleBufRef.current = new Float32Array(analyser.fftSize);

      transition({ type: 'engage' }); // idle → listening
      resumeListeningEffects(gen);
      if (!samplerRef.current) {
        samplerRef.current = setInterval(samplerTick, TALK_SAMPLE_INTERVAL_MS);
      }
    } catch (err) {
      // getUserMedia denied / AudioContext failure.
      if (gen !== genRef.current) return;
      disengage();
      cbRef.current.onEngageFailed?.(err instanceof Error ? err.message : String(err));
    }
  }, [supported, transition, resumeListeningEffects, samplerTick, disengage]);

  // Reply watcher: while awaiting-reply, a finished turn triggers TTS; a turn
  // error drops back to listening.
  const phase = useChatStore((s) => s.phase);
  useEffect(() => {
    if (statusRef.current !== 'awaiting-reply') return;
    const gen = genRef.current;
    if (phase === 'done') {
      const msgs = useChatStore.getState().messages;
      const latest = msgs[msgs.length - 1];
      if (!latest || latest.role !== 'assistant' || !latest.content.trim()) {
        recover(gen);
        return;
      }
      if (spokenIdRef.current === latest.id) return;
      spokenIdRef.current = latest.id;
      void speakReply(gen, latest.content);
    } else if (phase === 'error') {
      recover(gen);
    }
  }, [phase, recover, speakReply]);

  const toggle = useCallback(() => {
    if (statusRef.current === 'idle') {
      void engage();
    } else {
      disengage();
    }
  }, [engage, disengage]);

  // Release everything if the component unmounts mid-conversation.
  useEffect(() => {
    return () => {
      disengage();
    };
  }, [disengage]);

  return {
    status,
    active: status !== 'idle',
    supported,
    toggle,
    stop: disengage,
  };
}
