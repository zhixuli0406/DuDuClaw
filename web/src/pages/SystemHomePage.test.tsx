import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { useSystemStore } from '@/stores/system-store';
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
          <Route path="/app/system/causal" element={<div>causal-curation-page-probe</div>} />
          <Route path="/app/system/decision-lab" element={<div>decision-lab-page-probe</div>} />
          <Route path="/app/system/ccr" element={<div>ccr-page-probe</div>} />
          <Route path="/app/system/license" element={<div>license-page-probe</div>} />
          <Route path="/app/system/settings" element={<div>settings-page-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  mockWsClient.call.mockResolvedValue(null);
  // No `system.status` yet is the baseline every other test here runs under —
  // and with no `decision_enabled` field the Decision Lab card stays visible
  // (fail-open, see `useDecisionEnabled`).
  useSystemStore.setState({ status: null });
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

  it('renders the system page cards, reusing their existing labels', () => {
    renderHome();
    expect(screen.getByText(msg['nav.device'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.updates'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.accounts'])).toBeInTheDocument();
    expect(screen.getByText(msg['manage.security'])).toBeInTheDocument();
    expect(screen.getByText(msg['causalCuration.title'])).toBeInTheDocument();
    expect(screen.getByText(msg['decisionLab.title'])).toBeInTheDocument();
    expect(screen.getByText(msg['ccrDashboard.title'])).toBeInTheDocument();
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

  it('clicking the Causal Curation card opens its route', async () => {
    const user = userEvent.setup();
    renderHome();
    await user.click(screen.getByText(msg['causalCuration.title']));
    await waitFor(() => expect(screen.getByText('causal-curation-page-probe')).toBeInTheDocument());
  });

  // Regression: the three admin evidence/audit pages (Causal Curation,
  // Decision Lab, CCR Dashboard) had no `newIn` field anywhere — `SystemCardDef`
  // did not carry one and there was no nav-model row either, so a code review
  // flagged them as unable to ever show the 新功能 chip. `SystemCardDef` now
  // has an optional `newIn`, rendered the same way `LauncherPage`'s `AppCard`
  // does (`isNewFeature` + a `Badge`).
  it('shows the 新功能 chip on cards with a newIn set below the current version', () => {
    mockWsClient.call.mockResolvedValue(null);
    renderHome();
    // decisionLab/ccrDashboard/causalCuration all ship `newIn: '1.66.0'`;
    // with no reported system version, `isNewFeature` treats it as shown.
    const badges = screen.getAllByText(msg['nav.badge.new']);
    expect(badges.length).toBeGreaterThanOrEqual(3);
  });

  it('clicking Decision Lab opens its route', async () => {
    const user = userEvent.setup();
    renderHome();
    await user.click(screen.getByText(msg['decisionLab.title']));
    await waitFor(() => expect(screen.getByText('decision-lab-page-probe')).toBeInTheDocument());
  });

  it('clicking the CCR store card opens its route', async () => {
    const user = userEvent.setup();
    renderHome();
    await user.click(screen.getByText(msg['ccrDashboard.title']));
    await waitFor(() => expect(screen.getByText('ccr-page-probe')).toBeInTheDocument());
  });

  // X1 落日條款 (2026-09-29 audit): with `config.toml [decision] enabled =
  // false` every `/api/decision/*` route answers 404, so the card leaves the
  // grid with its nav row instead of offering a page that can only fail.
  it('drops the Decision Lab card when the operator disabled it', () => {
    useSystemStore.setState({ status: { decision_enabled: false } as never });
    renderHome();
    expect(screen.queryByText(msg['decisionLab.title'])).not.toBeInTheDocument();
    // Its section and neighbours are untouched.
    expect(screen.getByText(msg['systemHome.section.access'])).toBeInTheDocument();
    expect(screen.getByText(msg['causalCuration.title'])).toBeInTheDocument();
    expect(screen.getByText(msg['ccrDashboard.title'])).toBeInTheDocument();
  });

  it('keeps the Decision Lab card when the switch is on or unreported', () => {
    useSystemStore.setState({ status: { decision_enabled: true } as never });
    const { unmount } = renderHome();
    expect(screen.getByText(msg['decisionLab.title'])).toBeInTheDocument();
    unmount();
    // Older gateway: no field at all ⇒ visible (fail-open).
    useSystemStore.setState({ status: { version: '1.65.1' } as never });
    renderHome();
    expect(screen.getByText(msg['decisionLab.title'])).toBeInTheDocument();
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
