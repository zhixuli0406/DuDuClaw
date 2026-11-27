import { resetCompanyTimeZone, useCompanyTimeStore } from '@/lib/company-time';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { FilesPage } from './FilesPage';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';

const AGENTS = [{ name: 'sales', display_name: '業務小美', role: 'staff', status: 'active' }];

function jsonResponse(body: unknown) {
  return { ok: true, json: async () => body };
}

beforeEach(() => {
  resetCompanyTimeZone();
  vi.spyOn(api.agents, 'callable').mockImplementation(async () => ({ agents: AGENTS.map(a => ({ ...a, via: 'binding', selectable: true })), default_agent: null }) as never);
  vi.clearAllMocks();
  // Scoped viewers need a direct binding; team visibility does not grant files.
  useAuthStore.setState({
    user: { id: 'u1', display_name: 'Boss', role: 'manager' } as never,
    jwt: null,
    bindings: [{ user_id: 'u1', agent_name: 'sales', access_level: 'owner', bound_at: '' }],
  });
  useAgentsStore.setState({
    agents: AGENTS as never,
    loading: false,
    loaded: true,
    fetchAgents: vi.fn(),
  } as never);
});

afterEach(() => {
  resetCompanyTimeZone();
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

  it.each([
    ['Asia/Taipei', '2026-11-27', '2026-11-26T16:00:00Z', '2026-11-27T15:59:59.999Z'],
    ['America/New_York', '2026-03-08', '2026-03-08T05:00:00Z', '2026-03-09T03:59:59.999Z'],
  ])('sends inclusive company-zone bounds for %s on %s', async (zone, date, start, end) => {
    useCompanyTimeStore.setState({ status: 'set', zone });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ files: [] }));
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    fetchMock.mockClear();

    fireEvent.change(screen.getByLabelText('From date'), { target: { value: date } });

    await waitFor(() => {
      const call = fetchMock.mock.calls.at(-1);
      expect(String(call?.[0])).toContain('since=');
    });
    const url = new URL(String(fetchMock.mock.calls.at(-1)?.[0]), 'http://x');
    const since = Number(url.searchParams.get('since'));
    expect(since).toBe(Date.parse(start));
    expect(url.searchParams.get('until')).toBeNull();

    fireEvent.change(screen.getByLabelText('To date'), { target: { value: date } });
    await waitFor(() => {
      const filtered = new URL(String(fetchMock.mock.calls.at(-1)?.[0]), 'http://x');
      expect(Number(filtered.searchParams.get('until'))).toBe(Date.parse(end));
      expect(Number(filtered.searchParams.get('since'))).toBe(Date.parse(start));
    });
  });

  it('shows the honest "no files at all" empty state when nothing has ever landed', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse({ files: [] })));
    renderWithProviders(<FilesPage />);
    await waitFor(() => expect(screen.getByText('No files yet')).toBeInTheDocument());
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

  it('shows an empty state without fetching when the loaded roster has no selectable bucket', async () => {
    useAuthStore.setState({ user: { id: 'u1', display_name: 'Alice', role: 'employee' } as never, bindings: [] });
    useAgentsStore.setState({ agents: [], loading: false, loaded: true, fetchAgents: vi.fn() } as never);
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);
    await waitFor(() => expect(screen.getByText('No files yet')).toBeInTheDocument());
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('waits for the roster before resolving an empty view', async () => {
    useAgentsStore.setState({ agents: [], loaded: false });
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);
    expect(screen.queryByText('No files yet')).not.toBeInTheDocument();

    act(() => useAgentsStore.setState({ loaded: true }));
    await waitFor(() => expect(screen.getByText('No files yet')).toBeInTheDocument());
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each(['owner', 'operator', 'viewer'] as const)('loads an employee’s directly bound files with %s access', async (access_level) => {
    const user = userEvent.setup();
    useAuthStore.setState({
      user: { id: 'u1', display_name: 'Alice', role: 'employee' } as never,
      jwt: 'employee-token',
      bindings: [{ user_id: 'u1', agent_name: 'sales', access_level, bound_at: '' }],
    });
    useAgentsStore.setState({ agents: [...AGENTS, { ...AGENTS[0], name: 'private', display_name: 'Private worker' }] as never });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({
      files: [{ name: 'employee.pdf', size: 1024, mtime: 0, origin: 'uploaded' }],
    }));
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<FilesPage />);

    expect(await screen.findByText('employee.pdf')).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledWith('/api/files?agent=sales', {
      headers: { Authorization: 'Bearer employee-token' },
    });
    await user.click(screen.getByRole('combobox'));
    expect(await screen.findByRole('option', { name: '業務小美' })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Shared files' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Private worker' })).not.toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});

import { api } from '@/lib/api';
