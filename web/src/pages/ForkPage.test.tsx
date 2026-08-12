import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ForkPage } from './ForkPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ forks: [] });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('ForkPage', () => {
  it('renders the parallel-branches collection header', () => {
    renderWithProviders(<ForkPage />);
    expect(screen.getByRole('heading', { name: 'Forks' })).toBeInTheDocument();
  });

  it('offers a refresh action', () => {
    renderWithProviders(<ForkPage />);
    expect(screen.getByRole('button', { name: 'Refresh' })).toBeInTheDocument();
  });
});

// ── P11 (state-as-URL): the inspected fork is deep-linkable (`?fork=<id>`) ────

function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route
            path="/forks"
            element={
              <>
                <ForkPage />
                <SearchParamsProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

function SearchParamsProbe() {
  const [params] = useSearchParams();
  return <div data-testid="search-probe">{params.toString()}</div>;
}

/** A well-formed `fork.inspect` payload (the detail pane maps `branches`). */
const FORK_DETAIL = {
  fork_id: 'fork-abc',
  agent_id: 'nova',
  prompt: 'compare two drafts',
  merge_mode: 'judge',
  resolved: false,
  winner: null,
  branches: [],
};

describe('ForkPage — selected fork as URL (P11)', () => {
  beforeEach(() => {
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'fork.inspect' ? Promise.resolve(FORK_DETAIL) : Promise.resolve({ forks: [] }),
    );
  });

  it('inspects the fork named by ?fork= on load', async () => {
    renderAt('/forks?fork=fork-abc');
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('fork.inspect', { fork_id: 'fork-abc' }),
    );
  });

  it('does not inspect anything without the param', async () => {
    renderAt('/forks');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('fork.list', { limit: 50 }));
    expect(mockWsClient.call).not.toHaveBeenCalledWith('fork.inspect', expect.anything());
  });
});
