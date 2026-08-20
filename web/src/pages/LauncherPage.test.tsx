import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { LauncherPage } from './LauncherPage';
import { useAuthStore } from '@/stores/auth-store';
import { APPS, type AppManifest } from '@/apps/registry';

// en.json's inferred type is a giant literal-key object; index it dynamically
// (by app.<id>.name / app.<id>.desc, computed from the registry) via a widened
// view instead of an ever-growing union of string literals.
const msg = en as Record<string, string>;

function renderLauncher(apps?: readonly AppManifest[]) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/launcher']}>
        <Routes>
          <Route path="/launcher" element={<LauncherPage apps={apps} />} />
          <Route path="/chat" element={<div>chat-page-probe</div>} />
          <Route path="/agents" element={<div>agents-page-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

function setRole(role: 'admin' | 'manager' | 'employee') {
  useAuthStore.setState({
    user: { id: 'u1', email: 'x@x.com', display_name: 'X', role, status: 'active' },
  } as never);
}

beforeEach(() => {
  mockWsClient.call.mockResolvedValue(null);
  setRole('admin');
});

describe('LauncherPage — app grid driven by the registry', () => {
  it('renders every app the admin role can see (all seven)', async () => {
    renderLauncher();
    for (const app of APPS) {
      await waitFor(() => expect(screen.getByText(msg[app.nameId])).toBeInTheDocument());
    }
  });

  it('hides admin/manager-gated apps for an employee, keeps the ungated ones', async () => {
    setRole('employee');
    renderLauncher();
    await waitFor(() => expect(screen.getByText(msg['app.workbench.name'])).toBeInTheDocument());
    expect(screen.getByText(msg['app.staff.name'])).toBeInTheDocument();
    expect(screen.getByText(msg['app.memory.name'])).toBeInTheDocument();
    expect(screen.getByText(msg['app.files.name'])).toBeInTheDocument();
    expect(screen.queryByText(msg['app.system.name'])).not.toBeInTheDocument();
    expect(screen.queryByText(msg['app.comms.name'])).not.toBeInTheDocument();
    expect(screen.queryByText(msg['app.monitor.name'])).not.toBeInTheDocument();
  });

  it('a manager sees the manager-floor app (monitor) but not the admin-only ones', async () => {
    setRole('manager');
    renderLauncher();
    await waitFor(() => expect(screen.getByText(msg['app.monitor.name'])).toBeInTheDocument());
    expect(screen.queryByText(msg['app.system.name'])).not.toBeInTheDocument();
    expect(screen.queryByText(msg['app.comms.name'])).not.toBeInTheDocument();
  });

  it('shows the empty state when the role can see no apps at all', async () => {
    setRole('employee');
    renderLauncher([APPS[0]]); // 'system', admin-gated — invisible to employee
    await waitFor(() => expect(screen.getByText(msg['launcher.empty.title'])).toBeInTheDocument());
  });

  it('clicking a card navigates in-place to the app default path (SPA mode)', async () => {
    const user = userEvent.setup();
    renderLauncher();
    const chatCard = await screen.findByText(msg['app.workbench.name']);
    await user.click(chatCard);
    await waitFor(() => expect(screen.getByText('chat-page-probe')).toBeInTheDocument());
  });

  it('activating a card by keyboard (Enter) navigates too', async () => {
    const user = userEvent.setup();
    renderLauncher();
    const staffCard = await screen.findByText(msg['app.staff.name']);
    (staffCard.closest('[role="button"]') as HTMLElement | null)?.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(screen.getByText('agents-page-probe')).toBeInTheDocument());
  });

  it('renders the 新功能 badge for an app manifest carrying newIn (shared isNewFeature source, §3 C8)', async () => {
    const withNewApp: AppManifest[] = [
      { ...APPS[1], newIn: '99.0.0' }, // far-future version: always "new" regardless of the running build
    ];
    renderLauncher(withNewApp);
    await waitFor(() => expect(screen.getByText(msg['nav.badge.new'])).toBeInTheDocument());
  });

  it('does not render the badge for an app with no newIn (the default for all seven today)', async () => {
    renderLauncher([APPS[1]]);
    await waitFor(() => expect(screen.getByText(msg[APPS[1].nameId])).toBeInTheDocument());
    expect(screen.queryByText(msg['nav.badge.new'])).not.toBeInTheDocument();
  });
});
