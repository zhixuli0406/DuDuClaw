import { sanitizeErrorDetail } from './error-message';

const kinds = ['download', 'verify', 'extract', 'size', 'write', 'swap', 'rollback', 'unknown'] as const;
export type UpdateFailureKind = typeof kinds[number];

/** Accept old string RPC rejections, Error instances, and new structured failures. */
export function updateFailure(value: unknown): { kind: UpdateFailureKind; detail: string } {
  const record = value && typeof value === 'object' ? value as Record<string, unknown> : null;
  const detail = sanitizeErrorDetail(record?.detail ?? value, 2000);
  if (typeof record?.kind === 'string' && kinds.includes(record.kind as UpdateFailureKind)) {
    return { kind: record.kind as UpdateFailureKind, detail };
  }
  // Compatibility with gateways predating structured update errors (including 1.79.2).
  const text = detail.toLowerCase();
  let kind: UpdateFailureKind = 'unknown';
  if (text.includes('rollback failed')) kind = 'rollback';
  else if (/size limit|too large|size overflow/.test(text)) kind = 'size';
  else if (/staging write failed|failed to stage|failed to sync|failed to set permissions/.test(text)) kind = 'write';
  else if (/failed to backup|failed to install/.test(text)) kind = 'swap';
  else if (/^failed to read (update archive|checksum file|signature file):/.test(text)) kind = 'download';
  else if (/update verification failed:/.test(text)) kind = 'verify';
  else if (/download|http|connection|not connected|request timeout|websocket/.test(text)) kind = 'download';
  else if (/verification|cannot execute new binary|checksum|signature/.test(text)) kind = 'verify';
  else if (/archive|extract|zip entry|zip name/.test(text)) kind = 'extract';
  return { kind, detail };
}
