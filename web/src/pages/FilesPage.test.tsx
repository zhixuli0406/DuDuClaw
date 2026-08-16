import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { FilesPage } from './FilesPage';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';

const AGENTS = [{ name: 'sales', display_name: '業務小美', role: 'staff', status: 'active' }];

function jsonResponse(body: unknown) {
  return { ok: true, json: async () => body };
}

beforeEach(() => {
  vi.clearAllMocks();
  // `manager` scope passes the roster through unfiltered (`useVisibleAgents`)
  // while resolving straight to the first agent — `sales` — rather than the
  // admin-only shared bucket, so every test below gets a deterministic,
  // concrete `agent=sales` request without extra URL-state setup.
  useAuthStore.setState({ user: { display_name: 'Boss', role: 'manager' } as never, jwt: null, bindings: [] });
  useAgentsStore.setState({
    agents: AGENTS as never,
    loading: false,
    loaded: true,
    fetchAgents: vi.fn(),
  } as never);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('<FilesPage> — I-4 search / task / date filters', () => {
  it('debounces the search box into a `q=` round trip against /api/files', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ files: [] }));
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    fetchMock.mockClear();

    fireEvent.change(screen.getByPlaceholderText('Search file name or origin…'), {
      target: { value: 'quarterly' },
    });

    // Nothing fires immediately — the request is debounced.
    expect(fetchMock).not.toHaveBeenCalled();

    await waitFor(
      () => {
        const hit = fetchMock.mock.calls.find((c) => String(c[0]).includes('q=quarterly'));
        expect(hit).toBeTruthy();
      },
      { timeout: 2000 },
    );
  });

  it('sends since/until as inclusive epoch-millisecond bounds', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ files: [] }));
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    fetchMock.mockClear();

    fireEvent.change(screen.getByLabelText('From date'), { target: { value: '2026-08-01' } });

    await waitFor(() => {
      const call = fetchMock.mock.calls.at(-1);
      expect(String(call?.[0])).toContain('since=');
    });
    const url = new URL(String(fetchMock.mock.calls.at(-1)?.[0]), 'http://x');
    const since = Number(url.searchParams.get('since'));
    // Compared against the LOCAL-time constructor (not ISO/UTC slicing) so
    // this assertion holds regardless of the test runner's timezone — it must
    // match exactly what the component itself computes from the same
    // timezone-less date-time literal.
    expect(since).toBe(new Date(2026, 7, 1, 0, 0, 0).getTime());
    // Only `since` was set — no search/task param leaking in.
    expect(url.searchParams.get('until')).toBeNull();
  });

  it('shows the honest "no files at all" empty state when nothing has ever landed', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse({ files: [] })));
    renderWithProviders(<FilesPage />);
    expect(await screen.findByText('No files yet')).toBeInTheDocument();
  });

  it('shows the "filters matched nothing" empty state (not the generic one) once a search is active', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ files: [] }));
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());

    fireEvent.change(screen.getByPlaceholderText('Search file name or origin…'), {
      target: { value: 'nothing-matches-this' },
    });

    expect(await screen.findByText('No file matches the current filters')).toBeInTheDocument();
    expect(screen.queryByText('No files yet')).not.toBeInTheDocument();
  });

  it('renders returned files with their origin badge', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse({
          files: [
            { name: 'report.docx', size: 2048, mtime: Date.parse('2026-08-15T10:00:00Z'), origin: 'declared', task_id: 'task-42' },
          ],
        }),
      ),
    );
    renderWithProviders(<FilesPage />);
    expect(await screen.findByText('report.docx')).toBeInTheDocument();
    expect(screen.getByText('Delivered by AI staff')).toBeInTheDocument();
  });

  it('never calls fetch when no agent bucket is selectable yet', () => {
    useAgentsStore.setState({ agents: [], loading: false, loaded: true, fetchAgents: vi.fn() } as never);
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
