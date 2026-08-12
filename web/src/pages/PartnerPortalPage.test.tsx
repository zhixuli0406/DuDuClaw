import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { PartnerPortalPage } from './PartnerPortalPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // profile/stats/customers all resolve {} → empty profile → onboarding card.
  mockWsClient.call.mockResolvedValue({});
});

describe('PartnerPortalPage (MDS)', () => {
  it('renders the onboarding card when no partner profile exists', async () => {
    renderWithProviders(<PartnerPortalPage />);
    expect(await screen.findByText('Set up your partner profile')).toBeInTheDocument();
  });

  // X03 (UX audit §3.3): the Distributors cross-reference used to be plain,
  // unclickable text in the scope-disclaimer banner.
  it('opens the Distributors tab from the scope-note CrossLink', async () => {
    const user = userEvent.setup();

    function DistributorsProbe() {
      const [params] = useSearchParams();
      return <div>distributors-probe:{params.get('tab')}</div>;
    }

    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={['/manage/license']}>
          <Routes>
            <Route path="/manage/license" element={<PartnerPortalPage />} />
            <Route path="/manage/distributors" element={<DistributorsProbe />} />
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );

    const link = await screen.findByRole('button', { name: 'Go to Distributors' });
    await user.click(link);

    await waitFor(() => {
      expect(screen.getByText('distributors-probe:distributors')).toBeInTheDocument();
    });
  });
});
