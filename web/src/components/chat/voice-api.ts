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
export async function sttTranscribe(
  blob: Blob,
  filename = 'voice.webm',
  signal?: AbortSignal,
): Promise<string> {
  const form = new FormData();
  form.append('audio', blob, filename);

  const res = await fetch('/api/stt', {
    method: 'POST',
    headers: authHeader(),
    body: form,
    signal,
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
 * Zero-cost STT availability probe for Talk Mode (G13). `POST /api/stt`
 * resolves the configured provider *before* reading the multipart body, so an
 * empty form answers 501 when STT is unconfigured and 400 ("missing 'audio'")
 * when it is — without ever invoking the provider. Throws
 * {@link VoiceNotConfiguredError} on 501 (fail-closed: Talk Mode refuses to
 * engage); resolves on any other response.
 */
export async function sttPreflight(signal?: AbortSignal): Promise<void> {
  const res = await fetch('/api/stt', {
    method: 'POST',
    headers: authHeader(),
    body: new FormData(),
    signal,
  });
  if (res.status === 501) {
    throw new VoiceNotConfiguredError(await errorText(res));
  }
  // Any other status (expected: 400 missing audio) means STT is configured
  // enough to accept real segments; genuine failures surface on first use.
}

/**
 * Synthesize speech for `text` via `POST /api/tts`. Returns an object URL for an
 * `<audio>`/`Audio` element (caller must `URL.revokeObjectURL` when done).
 * Throws {@link VoiceNotConfiguredError} on 501 so the play toggle can close.
 */
export async function ttsSynthesizeUrl(
  text: string,
  voice = '',
  signal?: AbortSignal,
): Promise<string> {
  const res = await fetch('/api/tts', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeader() },
    body: JSON.stringify({ text, voice }),
    signal,
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
