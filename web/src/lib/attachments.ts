/**
 * Shared chat-attachment helpers (TODO-genspark-workspace-shell §P1.2).
 * Extracted from WebChatPage so the workspace PromptBar and the full WebChat
 * page apply identical size limits and base64 reading.
 */
import type { PendingAttachment } from '@/stores/chat-store';

/** 20 MB — must match the backend `media::MAX_FILE_SIZE` guard. */
export const MAX_ATTACHMENT_BYTES = 20 * 1024 * 1024;

export function isImageMime(mime: string): boolean {
  return mime.startsWith('image/');
}

/** Read a File into a base64 string (without the data: URI prefix). */
export function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const comma = result.indexOf(',');
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.onerror = () => reject(reader.error ?? new Error('read failed'));
    reader.readAsDataURL(file);
  });
}

export type ReadResult =
  | { ok: true; attachment: PendingAttachment }
  | { ok: false; reason: 'too-large' | 'read-failed'; name: string };

/**
 * Read a single file into a PendingAttachment, enforcing the size cap.
 * Returns a discriminated result so callers can surface localized errors.
 */
export async function readAttachment(file: File): Promise<ReadResult> {
  if (file.size > MAX_ATTACHMENT_BYTES) {
    return { ok: false, reason: 'too-large', name: file.name };
  }
  try {
    const dataBase64 = await readFileAsBase64(file);
    return {
      ok: true,
      attachment: {
        name: file.name,
        mime: file.type || 'application/octet-stream',
        dataBase64,
      },
    };
  } catch {
    return { ok: false, reason: 'read-failed', name: file.name };
  }
}
