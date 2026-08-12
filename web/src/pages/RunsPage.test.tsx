import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { RunsPage } from './RunsPage';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';

const AGENTS = [
  { name: 'nova', display_name: 'Nova', status: 'active', role: 'main', sandboxed: false },
  { name: 'agnes', display_name: 'Agnes', status: 'active', role: 'worker', sandboxed: false },
];

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ runs: [], agents: AGENTS });
  useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' }, bindings: [] } as never);
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAgentsStore.setState({ agents: [], loading: false } as never);
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('RunsPage', () => {
  it('renders the run-log header in the split layout', () => {
    renderWithProviders(<RunsPage />);
    expect(screen.getByRole('heading', { name: 'Run log' })).toBeInTheDocument();
  });

  it('shows the empty transcript prompt when no run is selected', () => {
    renderWithProviders(<RunsPage />);
    expect(screen.getByText('Pick a run on the left')).toBeInTheDocument();
  });
});

// ── W3-3: state-as-URL (Stripe pattern B4) — the agent filter is
// bookmarkable/shareable.

function SearchParamsProbe() {
  const [params] = useSearchParams();
  return <div data-testid="search-probe">{params.toString()}</div>;
}

/** `renderWithProviders`'s router has no way to seed a starting query string. */
function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route
            path="/runs"
            element={
              <>
                <RunsPage />
                <SearchParamsProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('RunsPage — agent filter as URL (W3-3)', () => {
  it('seeds the agent filter from ?agent= on load', async () => {
    renderAt('/runs?agent=agnes');
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('runs.list', { agent_id: 'agnes' });
    });
  });

  it('mirrors the seeded filter back into the URL (round-trip, no stale param left behind)', async () => {
    renderAt('/runs?agent=agnes');
    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('agent=agnes');
    });
  });

  it('does not write an "agent" param when no filter is selected', async () => {
    renderAt('/runs');
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Run log' })).toBeInTheDocument();
    });
    expect(screen.getByTestId('search-probe')).not.toHaveTextContent('agent=');
  });
});

// ── P11: the open transcript is deep-linkable too (`?run=<id>`) ───────────────

/** A minimal but well-formed `runs.get` payload (the page reads `detail.run`). */
const RUN_DETAIL = {
  run: {
    id: 'run-42',
    agent_id: 'nova',
    channel: 'webchat',
    started_at: '2026-08-12T00:00:00Z',
    status: 'ok',
  },
  events: [],
};

describe('RunsPage — selected run as URL (P11)', () => {
  beforeEach(() => {
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'runs.get'
        ? Promise.resolve(RUN_DETAIL)
        : Promise.resolve({ runs: [], agents: AGENTS }),
    );
  });

  it('fetches the transcript named by ?run= on load', async () => {
    renderAt('/runs?run=run-42');
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('runs.get', { run_id: 'run-42' });
    });
  });

  it('shows the transcript pane instead of the "pick a run" prompt', async () => {
    renderAt('/runs?run=run-42');
    await waitFor(() => {
      expect(screen.queryByText('Pick a run on the left')).not.toBeInTheDocument();
    });
  });

  it('writes the selected run back into the URL when a row is clicked', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'runs.list'
        ? Promise.resolve({
            runs: [
              {
                id: 'run-7',
                agent_id: 'nova',
                channel: 'webchat',
                started_at: new Date().toISOString(),
                status: 'ok',
              },
            ],
            agents: AGENTS,
          })
        : Promise.resolve({ run: {}, events: [] }),
    );
    renderAt('/runs');

    const row = await screen.findByRole('button', { name: /run-7|Nova/i });
    await user.click(row);
    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('run=run-7');
    });
  });
});
