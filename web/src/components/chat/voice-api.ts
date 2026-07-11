/**
 * HTTP helpers for the voice endpoints (openhuman-parity B). Kept local to the
 * chat feature rather than the WS-based `@/lib/api` client, since STT/TTS are
 * plain REST (multipart in, audio/JSON out) authenticated with the same Bearer
 * JWT the dashboard already holds.
 */
import { useAuthStore } from '@/stores/auth-store';

/** Thrown when the gateway reports STT/TTS is not configured (HTTP 501). */
export class VoiceNotConfiguredError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'VoiceNotConfiguredError';
  }
}

function authHeader(): Record<string, string> {
  const jwt = useAuthStore.getState().jwt;
  return jwt ? { Authorization: `Bearer ${jwt}` } : {};
}

async function errorText(res: Response): Promise<string> {
  try {
    const body = await res.json();
    if (body && typeof body.error === 'string') return body.error;
  } catch {
    /* not JSON */
  }
  return `HTTP ${res.status}`;
}

/**
 * Transcribe a recorded audio blob via `POST /api/stt`. Returns the recognized
 * text. Throws {@link VoiceNotConfiguredError} on 501 so callers can guide the
 * user to settings instead of showing a generic failure.
 */
export async function sttTranscribe(blob: Blob, filename = 'voice.webm'): Promise<string> {
  const form = new FormData();
  form.append('audio', blob, filename);

  const res = await fetch('/api/stt', {
    method: 'POST',
    headers: authHeader(),
    body: form,
  });

  if (res.status === 501) {
    throw new VoiceNotConfiguredError(await errorText(res));
  }
  if (!res.ok) {
    throw new Error(await errorText(res));
  }
  const data = await res.json();
  return typeof data?.text === 'string' ? data.text : '';
}

/**
 * Synthesize speech for `text` via `POST /api/tts`. Returns an object URL for an
 * `<audio>`/`Audio` element (caller must `URL.revokeObjectURL` when done).
 * Throws {@link VoiceNotConfiguredError} on 501 so the play toggle can close.
 */
export async function ttsSynthesizeUrl(text: string, voice = ''): Promise<string> {
  const res = await fetch('/api/tts', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeader() },
    body: JSON.stringify({ text, voice }),
  });

  if (res.status === 501) {
    throw new VoiceNotConfiguredError(await errorText(res));
  }
  if (!res.ok) {
    throw new Error(await errorText(res));
  }
  const audioBlob = await res.blob();
  return URL.createObjectURL(audioBlob);
}
