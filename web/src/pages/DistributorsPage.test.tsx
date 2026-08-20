import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DistributorsPage } from './DistributorsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // The page fires distributor.status() + distributor.list() via Promise.all —
  // one resolved value satisfies both calls.
  mockWsClient.call.mockResolvedValue({
    issuer_configured: false,
    stats: null,
    distributors: [],
  });
});

describe('DistributorsPage', () => {
  it('renders the page header', () => {
    renderWithProviders(<DistributorsPage />);
    // en.json → "manage.distributors": "White-label"
    expect(screen.getByRole('heading', { name: 'White-label' })).toBeInTheDocument();
  });

  // X03 (UX audit §3.3): the Partner cross-reference used to be plain,
  // unclickable text on the 經銷商簽發 tab. N-3 (2026-08): the target
  // relocated from `/manage/license` to `/app/system/license`.
  it('opens the Partner tab from the distributors-tab scope note', async () => {
    const user = userEvent.setup();

    function PartnerProbe() {
      const [params] = useSearchParams();
      return <div>partner-probe:{params.get('tab')}</div>;
    }

    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={['/manage/distributors?tab=distributors']}>
          <Routes>
            <Route path="/manage/distributors" element={<DistributorsPage />} />
            <Route path="/app/system/license" element={<PartnerProbe />} />
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );

    const link = await screen.findByRole('button', { name: 'Go to Partner' });
    await user.click(link);

    await waitFor(() => {
      expect(screen.getByText('partner-probe:partner')).toBeInTheDocument();
    });
  });
});

// ── P11 (state-as-URL): the tab used to be read once at mount and never
// written back, so switching tabs and refreshing always dumped you on 品牌設定.

describe('DistributorsPage — tab as URL (P11)', () => {
  function TabProbe() {
    const [params] = useSearchParams();
    return <div data-testid="tab-probe">{params.get('tab') ?? ''}</div>;
  }

  function renderAt(path: string) {
    return render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={[path]}>
          <Routes>
            <Route
              path="/manage/distributors"
              element={
                <>
                  <DistributorsPage />
                  <TabProbe />
                </>
              }
            />
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );
  }

  it('opens on the tab named by ?tab=', async () => {
    renderAt('/manage/distributors?tab=distributors');
    expect(await screen.findByRole('tab', { selected: true })).toHaveTextContent(/distributor/i);
  });

  it('writes the tab back into the URL when it is switched', async () => {
    const user = userEvent.setup();
    renderAt('/manage/distributors');
    expect(screen.getByTestId('tab-probe')).toHaveTextContent('');

    const tabs = screen.getAllByRole('tab');
    await user.click(tabs[1]);
    await waitFor(() => expect(screen.getByTestId('tab-probe')).toHaveTextContent('distributors'));
  });

  it('falls back to the default tab for an unknown ?tab= value', async () => {
    renderAt('/manage/distributors?tab=nonsense');
    expect(await screen.findByRole('tab', { selected: true })).toHaveTextContent(/brand/i);
  });
});
