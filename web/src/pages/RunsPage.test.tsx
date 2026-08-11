import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
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
