import { setAgentRequestAuthorizer } from '../agent-request-guard';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('@/stores/auth-store', () => ({ useAuthStore: { getState: () => ({ jwt: 'dashboard-session' }) } }));
import { meetingDriveApi } from './drive';
beforeEach(() => setAgentRequestAuthorizer({
    ensureFreshGrants: async () => {},
    canSelectAgent: name => name === 'alice',
    identity: () => 'meeting-test',
  }));
afterEach(() => vi.unstubAllGlobals());
describe('meeting Drive dashboard API', () => {
  it('registers the exact file and employee with dashboard authentication and explicit confirmation', async () => {
    const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ selection_receipt: 'receipt', expires_in: 300 }) });
    vi.stubGlobal('fetch', fetcher);
    const receipt = await meetingDriveApi.registerSelection('alice', 'file-123456789');
    const [path, request] = fetcher.mock.calls[0];
    expect(path).toBe('/api/meeting-drive/selection');
    expect(request.headers.get('Authorization')).toBe('Bearer dashboard-session');
    expect(request.cache).toBe('no-store');
    expect(JSON.parse(request.body)).toEqual({ agent_id: 'alice', file_id: 'file-123456789', confirm: true });
    expect(receipt.selection_receipt).toBe('receipt');
  });
  it('sends only the selected file and employee with explicit confirmation', async () => {
    const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ job_id: 'job' }) });
    vi.stubGlobal('fetch', fetcher);
    const abort = new AbortController();
    await meetingDriveApi.import('alice', 'file-123456789', 'selection-receipt', abort.signal);
    const [path, request] = fetcher.mock.calls[0];
    expect(path).toBe('/api/meeting-drive/import');
    expect(request.headers.get('Authorization')).toBe('Bearer dashboard-session');
    expect(JSON.parse(request.body)).toEqual({ agent_id: 'alice', file_id: 'file-123456789', selection_receipt: 'selection-receipt', confirm: true });
    expect(request.signal).toBe(abort.signal);
  });
  it('encodes search and pagination as data and disables cached personal lists', async () => {
    const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ files: [] }) });
    vi.stubGlobal('fetch', fetcher);
    await meetingDriveApi.files('alice', "a' & b", 'page/token');
    const url = new URL(fetcher.mock.calls[0][0], 'https://dashboard.example');
    expect(url.searchParams.get('search')).toBe("a' & b");
    expect(url.searchParams.get('page_token')).toBe('page/token');
    expect(fetcher.mock.calls[0][1].cache).toBe('no-store');
  });
});
