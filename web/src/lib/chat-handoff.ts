import type { PendingAttachment } from '@/stores/chat-store';

/**
 * 問一問 handoff of files from the 交辦任務 page to `/chat`.
 *
 * The draft text rides in router state; the files do not. Router state is
 * written into the browser's session history, which caps its size (Firefox
 * refuses state over 16 MiB, and React Router then falls back to a full page
 * load that drops the state entirely) — ten 20 MB files in base64 would never
 * fit. So the files wait here, in memory, and router state carries only the
 * one-time key. A key is consumed once, so a back-navigation or a refresh
 * cannot resurrect files that were already handed over.
 */

const pending = new Map<string, readonly PendingAttachment[]>();

/** Keep `files` for the chat page; returns the key to put in router state. */
export function stashChatAttachments(files: readonly PendingAttachment[]): string {
  const key = typeof crypto !== 'undefined' && 'randomUUID' in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
  // Only the latest handoff is ever wanted; never let abandoned ones pile up.
  pending.clear();
  pending.set(key, files);
  return key;
}

/** Take (and forget) the files stored under `key`; `[]` when unknown. */
export function takeChatAttachments(key: unknown): PendingAttachment[] {
  if (typeof key !== 'string') return [];
  const files = pending.get(key);
  pending.delete(key);
  return files ? [...files] : [];
}
