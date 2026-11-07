import { setAgentRequestAuthorizer } from '../agent-request-guard';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('@/stores/auth-store', () => ({ useAuthStore: { getState: () => ({ jwt: 'session-token' }) } }));
import { uploadMeeting, uploadStorageKey } from './api';
const response = (value: unknown, status = 200) => ({ ok: status < 400, status, json: async () => value }) as Response;
function recording(): File {
  const bytes = new TextEncoder().encode('abcdefghij');
  return { name: 'meeting.wav', size: bytes.length, lastModified: 42,
    slice: (start: number, end: number) => ({ arrayBuffer: async () => bytes.slice(start, end).buffer }),
  } as File;
}
beforeEach(() => {
  setAgentRequestAuthorizer({
    ensureFreshGrants: async () => {},
    canSelectAgent: name => name === 'alice',
    identity: () => 'meeting-test',
  });
  localStorage.clear();
  vi.stubGlobal('crypto', { subtle: { digest: vi.fn().mockResolvedValue(new Uint8Array([1, 2]).buffer) } });
});
afterEach(() => vi.unstubAllGlobals());
describe('resumable meeting upload', () => {
  it('resumes only missing chunks, verifies prior hashes, reports byte progress and sends auth', async () => {
    const file = recording();
    const key = uploadStorageKey('person', 'alice', file);
    localStorage.setItem(key, 'upload-one');
    const fetcher = vi.fn().mockResolvedValueOnce(response({ upload_id: 'upload-one', chunk_size: 4, received: [0], hashes: { 0: '0102' } }))
      .mockResolvedValueOnce(response({})).mockResolvedValueOnce(response({})).mockResolvedValueOnce(response({ job_id: 'job-one', state: 'queued' }));
    vi.stubGlobal('fetch', fetcher);
    const progress: number[] = [];
    expect((await uploadMeeting(file, 'alice', 'person', value => progress.push(value))).job_id).toBe('job-one');
    expect(fetcher.mock.calls.map(call => call[0])).toEqual(['/api/meeting-uploads/upload-one', '/api/meeting-uploads/upload-one/1', '/api/meeting-uploads/upload-one/2', '/api/meeting-uploads/upload-one/complete']);
    expect(progress).toEqual([0.4, 0.8, 1]);
    expect(fetcher.mock.calls[1][1].headers.get('X-Chunk-SHA256')).toBe('0102');
    expect(fetcher.mock.calls[1][1].headers.get('Authorization')).toBe('Bearer session-token');
    expect(localStorage.getItem(key)).toBeNull();
  });
  it('refuses changed content before appending to a resumed upload', async () => {
    const file = recording(); localStorage.setItem(uploadStorageKey('p', 'alice', file), 'upload-one');
    const fetcher = vi.fn().mockResolvedValue(response({ upload_id: 'upload-one', chunk_size: 4, received: [0], hashes: { 0: 'different' } }));
    vi.stubGlobal('fetch', fetcher);
    await expect(uploadMeeting(file, 'alice', 'p', () => {})).rejects.toThrow('meeting.fileChanged');
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
  it('creates a fresh upload after expiry and preserves its receipt on connection failure', async () => {
    const file = recording(); const key = uploadStorageKey('p', 'alice', file); localStorage.setItem(key, 'expired');
    const fetcher = vi.fn().mockResolvedValueOnce(response({}, 410))
      .mockResolvedValueOnce(response({ upload_id: 'fresh', chunk_size: 4, received: [] }))
      .mockRejectedValueOnce(new Error('offline'));
    vi.stubGlobal('fetch', fetcher);
    await expect(uploadMeeting(file, 'alice', 'p', () => {})).rejects.toThrow('offline');
    expect(localStorage.getItem(key)).toBe('fresh');
    expect(JSON.parse(fetcher.mock.calls[1][1].body)).toEqual({ agent_id: 'alice', file_name: 'meeting.wav', size: 10 });
    expect(uploadStorageKey('other', 'alice', file)).not.toBe(key);
    expect(uploadStorageKey('p', 'bob', file)).not.toBe(key);
  });
});
