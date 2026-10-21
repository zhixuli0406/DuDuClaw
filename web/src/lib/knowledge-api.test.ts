import { describe, it, expect, vi, afterEach } from 'vitest';
import { downloadOriginal, precheckFile, REVOKE_DELAY_MS } from './knowledge-api';

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('knowledge-api helpers', () => {
  it('precheckFile uses the gateway limits and passes everything without them', () => {
    const limits = { max_source_bytes: 100, max_files_per_upload: 20, extensions: ['pdf'] };
    expect(precheckFile({ name: 'A.PDF', size: 10 }, limits)).toBeNull();
    expect(precheckFile({ name: 'a.doc', size: 10 }, limits)).toBe('type');
    expect(precheckFile({ name: 'noext', size: 10 }, limits)).toBe('type');
    expect(precheckFile({ name: 'a.pdf', size: 101 }, limits)).toBe('size');
    expect(precheckFile({ name: 'a.exe', size: 10 ** 9 }, null)).toBeNull();
  });

  it('keeps the download URL alive after the click', async () => {
    vi.useFakeTimers();
    const revoke = vi.fn();
    vi.stubGlobal('URL', { ...URL, createObjectURL: () => 'blob:x', revokeObjectURL: revoke });
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(new Blob(['x']), { status: 200 })));
    await downloadOriginal({ id: 's1', title: 't', original_name: 'a.pdf', format: 'pdf' });
    expect(revoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(REVOKE_DELAY_MS);
    expect(revoke).toHaveBeenCalledWith('blob:x');
  });
});
