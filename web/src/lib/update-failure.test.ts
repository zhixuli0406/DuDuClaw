import { describe, it, expect } from 'vitest';
import { updateFailure } from './update-failure';

describe('update failure compatibility', () => {
  it.each([
    ['Extracted binary exceeds size limit (possible zip bomb)', 'size'],
    ['Update archive download failed: reset', 'download'],
    ['Failed to read Update archive: reset', 'download'],
    ['Failed to read Checksum file: reset', 'download'],
    ['Failed to read Signature file: reset', 'download'],
    ['Checksum file download returned HTTP 503', 'download'],
    ['SHA-256 checksum mismatch', 'verify'],
    ['Archive entry error: corrupt', 'extract'],
    ['Staging write failed: disk full', 'write'],
    ['Failed to install new binary (rolled back successfully): denied', 'swap'],
    ['Failed to install AND rollback failed: denied', 'rollback'],
  ])('preserves old gateway string %s', (message, kind) => {
    expect(updateFailure(message)).toEqual({ kind, detail: message });
    expect(updateFailure(new Error(message))).toEqual({ kind, detail: message });
    expect(updateFailure({ message })).toEqual({ kind, detail: message });
  });
  it('uses structured kinds and always has a fallback', () => {
    expect(updateFailure({ kind: 'extract', message: 'opaque provider error' }).kind).toBe('extract');
    expect(updateFailure(undefined)).toEqual({ kind: 'unknown', detail: '' });
    expect(updateFailure({ kind: 'future', detail: 'opaque' })).toEqual({ kind: 'unknown', detail: 'opaque' });
  });
});
