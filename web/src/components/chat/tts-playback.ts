/**
 * Reply-playback toggle persistence (openhuman-parity B-P2).
 *
 * The chat header exposes a small speaker toggle; when on, each completed
 * assistant reply is spoken via `POST /api/tts`. The preference is remembered
 * in `localStorage` under `duduclaw:chat:tts`. Pure helpers so the persistence
 * is unit-testable with an injected storage.
 */

export const TTS_TOGGLE_KEY = 'duduclaw:chat:tts';

function storageOrNull(injected?: Storage): Storage | null {
  if (injected) return injected;
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    // Access can throw in privacy modes / sandboxed frames.
    return null;
  }
}

/** Read the persisted toggle. Defaults to `false` (off) when unset or on error. */
export function loadTtsEnabled(injected?: Storage): boolean {
  const s = storageOrNull(injected);
  if (!s) return false;
  try {
    return s.getItem(TTS_TOGGLE_KEY) === '1';
  } catch {
    return false;
  }
}

/** Persist the toggle. Silently no-ops when storage is unavailable. */
export function saveTtsEnabled(enabled: boolean, injected?: Storage): void {
  const s = storageOrNull(injected);
  if (!s) return;
  try {
    s.setItem(TTS_TOGGLE_KEY, enabled ? '1' : '0');
  } catch {
    /* ignore quota / access errors */
  }
}
