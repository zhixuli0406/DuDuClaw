/** Only server-issued references can become history file cards. */
export interface HistoryAttachment {
  readonly id: string;
  readonly name: string;
  readonly size: number;
  readonly mime: string;
}

export const HISTORY_PREVIEW_MAX_BYTES = 2 * 1024 * 1024;
export const HISTORY_DOWNLOAD_MAX_BYTES = 20 * 1024 * 1024;

/** Bound memory even if a stale response length or a growing file is streamed. */
export async function readHistoryAttachmentBlob(response: Response, maxBytes: number, signal: AbortSignal): Promise<Blob> {
  const length = response.headers.get('content-length');
  const size = length !== null && /^\d+$/.test(length) ? Number(length) : NaN;
  if (!Number.isSafeInteger(size) || size > maxBytes || signal.aborted || !response.body) {
    await response.body?.cancel();
    throw new Error('attachment unavailable');
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array<ArrayBuffer>[] = [];
  let received = 0;
  try {
    while (true) {
      if (signal.aborted) throw new Error('session changed');
      const { done, value } = await reader.read();
      if (done) break;
      received += value.byteLength;
      if (received > maxBytes || received > size) throw new Error('attachment too large');
      chunks.push(new Uint8Array(value));
    }
    if (received !== size || signal.aborted) throw new Error('attachment unavailable');
    return new Blob(chunks, { type: response.headers.get('content-type') ?? 'application/octet-stream' });
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    throw error;
  } finally {
    reader.releaseLock();
  }
}

export function parseHistoryAttachments(raw: unknown): HistoryAttachment[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter((item): item is HistoryAttachment =>
    item !== null && typeof item === 'object'
    && typeof item.id === 'string' && item.id.length > 0
    && typeof item.name === 'string' && item.name.length > 0
    && typeof item.mime === 'string'
    && typeof item.size === 'number' && Number.isSafeInteger(item.size) && item.size >= 0
  ).map(({ id, name, size, mime }) => ({ id, name, size, mime }));
}

// Exclude SVG and arbitrary image/* payloads from inline rendering.
export function isHistoryImage(mime: string): boolean {
  return ['image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/avif', 'image/bmp'].includes(mime);
}

/** Prefer the server's actual filename over the legacy Markdown display label. */
export function historyDownloadName(disposition: string | null, fallback: string): string {
  const basename = (name: string) => {
    const leaf = name.split(/[\\/]/).pop() ?? '';
    return leaf && leaf !== '.' && leaf !== '..' && !/[\u0000-\u001f\u007f]/.test(leaf) ? leaf : 'attachment';
  };
  if (!disposition) return basename(fallback);
  const encoded = /(?:^|;)\s*filename\*\s*=\s*UTF-8''([^;]*)/i.exec(disposition);
  if (encoded) {
    try { return basename(decodeURIComponent(encoded[1].trim())); }
    catch { return 'attachment'; }
  }
  const plain = /(?:^|;)\s*filename\s*=\s*(?:"([^"\r\n]*)"|([^;]*))/i.exec(disposition);
  return plain ? basename((plain[1] ?? plain[2]).trim()) : 'attachment';
}
