import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { useLocation } from 'react-router';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { CommandPalette } from './CommandPalette';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useTasksStore } from '@/stores/tasks-store';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

/**
 * W3-3 (Stripe pattern B4): pasting an object id into the command palette
 * jumps straight to its detail view. `tasks`/`agents` are deliberately kept
 * empty in the local stores below so the palette's ordinary local-command
 * search cannot match — that forces every scenario through the actual
 * ID-lookup RPC fan-out (`api.tasks.list` / `approvals.list` /
 * `install_requests.list` / `chat.sessions.list`) instead of a store hit.
 */

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location-probe">{location.pathname}{location.search}</div>;
}

function renderPalette() {
  return renderWithProviders(
    <>
      <CommandPalette />
      <LocationProbe />
    </>,
  );
}

/** Route each RPC the id-lookup fan-out issues; defaults to "nothing here". */
function mockIdLookupCalls(overrides: Partial<Record<string, unknown>> = {}) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method in overrides) return Promise.resolve(overrides[method]);
    switch (method) {
      case 'tasks.list':
        return Promise.resolve({ tasks: [] });
      case 'approvals.list':
        return Promise.resolve({ approvals: [], count: 0 });
      case 'install_requests.list':
        return Promise.resolve({ requests: [], count: 0 });
      case 'chat.sessions.list':
        return Promise.resolve({ sessions: [] });
      default:
        return Promise.resolve({});
    }
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  useCommandPaletteStore.setState({ open: true, recent: [] });
  useAgentsStore.setState({ agents: [], loading: false } as never);
  useTasksStore.setState({ tasks: [], loading: false } as never);
  useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' }, bindings: [] } as never);
  useSystemStore.setState({ status: null } as never);
  mockIdLookupCalls();
});

describe('CommandPalette — ID direct-jump (W3-3)', () => {
  it('shows the paste-an-id hint in the search placeholder', () => {
    renderPalette();
    expect(screen.getByPlaceholderText(/Paste an ID/i)).toBeInTheDocument();
  });

  it('jumps straight to a task detail when the pasted id matches a task (RPC fan-out, not the local store)', async () => {
    mockIdLookupCalls({
      'tasks.list': {
        tasks: [
          {
            id: 'qzx_7231_vwnp',
            title: 'Ship the release',
            description: '',
            status: 'todo',
            priority: 'medium',
            assigned_to: null,
            created_by: 'user',
            created_at: '2026-08-01T00:00:00Z',
            updated_at: '2026-08-01T00:00:00Z',
            tags: [],
          },
        ],
      },
    });
    renderPalette();
    const input = screen.getByPlaceholderText(/Paste an ID/i);
    fireEvent.change(input, { target: { value: 'qzx_7231_vwnp' } });

    await waitFor(
      () => expect(screen.getByTestId('location-probe')).toHaveTextContent('/tasks/qzx_7231_vwnp'),
      { timeout: 3000 },
    );
    // The palette closes itself on a hit.
    expect(useCommandPaletteStore.getState().open).toBe(false);
  });

  it('jumps to /inbox?item=<id> when the pasted id matches an approval', async () => {
    mockIdLookupCalls({
      'approvals.list': {
        approvals: [
          {
            id: 'qzx_7231_vwnp',
            agent_id: 'nova',
            kind: 'tool_call',
            summary: 'Run a script',
            payload: {},
            created_at: '2026-08-01T00:00:00Z',
            ttl_seconds: 3600,
          },
        ],
        count: 1,
      },
    });
    renderPalette();
    const input = screen.getByPlaceholderText(/Paste an ID/i);
    fireEvent.change(input, { target: { value: 'qzx_7231_vwnp' } });

    await waitFor(
      () => expect(screen.getByTestId('location-probe')).toHaveTextContent('/inbox?item=qzx_7231_vwnp'),
      { timeout: 3000 },
    );
  });

  it('shows a "not found" state when every source misses', async () => {
    renderPalette();
    const input = screen.getByPlaceholderText(/Paste an ID/i);
    fireEvent.change(input, { target: { value: 'qzx_7231_vwnp' } });

    await waitFor(
      () => expect(screen.getByText('No match for "qzx_7231_vwnp"')).toBeInTheDocument(),
      { timeout: 3000 },
    );
    // Never navigated away.
    expect(screen.getByTestId('location-probe')).toHaveTextContent('/');
  });

  it('does not spend an RPC round-trip on an ordinary natural-language query', async () => {
    renderPalette();
    const input = screen.getByPlaceholderText(/Paste an ID/i);
    fireEvent.change(input, { target: { value: 'open tasks board' } });

    // Give any (incorrect) debounce a chance to fire, then confirm no
    // id-lookup RPC was issued for this phrase.
    await new Promise((r) => setTimeout(r, 300));
    const idLookupMethods = ['tasks.list', 'approvals.list', 'install_requests.list', 'chat.sessions.list'];
    for (const call of mockWsClient.call.mock.calls) {
      expect(idLookupMethods).not.toContain(call[0]);
    }
  });
});
