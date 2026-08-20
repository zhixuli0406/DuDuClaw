import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { SystemHomePage } from './SystemHomePage';

const msg = en as Record<string, string>;

/**
 * SystemHomePage — `/app/system` bare index (N-3,
 * `DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-3). The settings hub the
 * 系統設定 app's `defaultPath` now points at; each card is a plain nav-away
 * (SPA mode) to one of the six migrated canonical routes.
 */
function renderHome() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/app/system']}>
        <Routes>
          <Route path="/app/system" element={<SystemHomePage />} />
          <Route path="/app/system/device" element={<div>device-page-probe</div>} />
          <Route path="/app/system/updates" element={<div>updates-page-probe</div>} />
          <Route path="/app/system/accounts" element={<div>accounts-page-probe</div>} />
          <Route path="/app/system/security" element={<div>security-page-probe</div>} />
          <Route path="/app/system/license" element={<div>license-page-probe</div>} />
          <Route path="/app/system/settings" element={<div>settings-page-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  mockWsClient.call.mockResolvedValue(null);
});

describe('SystemHomePage — settings hub (N-3)', () => {
  it('renders the page header with the app name and description', () => {
    renderHome();
    expect(screen.getByText(msg['app.system.name'])).toBeInTheDocument();
  });

  it('renders all three sections', () => {
    renderHome();
    expect(screen.getByText(msg['systemHome.section.hardware'])).toBeInTheDocument();
    expect(screen.getByText(msg['systemHome.section.access'])).toBeInTheDocument();
    expect(screen.getByText(msg['systemHome.section.general'])).toBeInTheDocument();
  });

  it('renders all six page cards, reusing the pages\' own nav-model labels', () => {
    renderHome();
    expect(screen.getByText(msg['nav.device'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.updates'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.accounts'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.security'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.license'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.system'])).toBeInTheDocument();
  });

  it('clicking the 裝置 card navigates to /app/system/device', async () => {
    const user = userEvent.setup();
    renderHome();
    await user.click(screen.getByText(msg['nav.device']));
    await waitFor(() => expect(screen.getByText('device-page-probe')).toBeInTheDocument());
  });

  it('clicking the 授權 card navigates to /app/system/license', async () => {
    const user = userEvent.setup();
    renderHome();
    await user.click(screen.getByText(msg['manage.license']));
    await waitFor(() => expect(screen.getByText('license-page-probe')).toBeInTheDocument());
  });

  it('activating the 設定 card by keyboard (Enter) navigates too', async () => {
    const user = userEvent.setup();
    renderHome();
    const card = screen.getByText(msg['manage.system']);
    (card.closest('[role="button"]') as HTMLElement | null)?.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(screen.getByText('settings-page-probe')).toBeInTheDocument());
  });
});
