import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router';
import { IntlProvider } from 'react-intl';
import '@/test/mocks';
import en from '@/i18n/en.json';
import { FirstRunGate } from './FirstRunGate';
import { useAgentsStore } from '@/stores/agents-store';

function renderGate() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/']}>
        <Routes>
          <Route element={<FirstRunGate />}>
            <Route index element={<div>DASHBOARD</div>} />
          </Route>
          <Route path="/welcome" element={<div>WELCOME</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({ agents: [], loading: false, loaded: false, error: null });
});

describe('FirstRunGate', () => {
  it('shows a spinner before the agent list has loaded (no premature redirect)', () => {
    useAgentsStore.setState({ loaded: false, loading: true });
    renderGate();
    expect(screen.getByRole('status')).toBeInTheDocument();
    expect(screen.queryByText('WELCOME')).not.toBeInTheDocument();
    expect(screen.queryByText('DASHBOARD')).not.toBeInTheDocument();
  });

  it('redirects to /welcome when loaded with zero agents', () => {
    useAgentsStore.setState({ loaded: true, loading: false, agents: [] });
    renderGate();
    expect(screen.getByText('WELCOME')).toBeInTheDocument();
    expect(screen.queryByText('DASHBOARD')).not.toBeInTheDocument();
  });

  it('renders the app when at least one agent exists', () => {
    useAgentsStore.setState({
      loaded: true,
      loading: false,
      agents: [{ name: 'bot', display_name: 'Bot', status: 'active' }] as never[],
    });
    renderGate();
    expect(screen.getByText('DASHBOARD')).toBeInTheDocument();
    expect(screen.queryByText('WELCOME')).not.toBeInTheDocument();
  });
});

/**
 * N-3 (2026-08, `DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-3): the
 * license page relocated from `/license` to `/app/system/license` — the
 * welcome wizard's zero-agent "unlock a Pro license" CTA must reach it
 * without bouncing back to `/welcome` (a dead loop this gate exists to
 * avoid), for BOTH the new canonical path and the still-working legacy
 * redirect alias.
 */
function renderGateAt(initialPath: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[initialPath]}>
        <Routes>
          <Route element={<FirstRunGate />}>
            <Route index element={<div>DASHBOARD</div>} />
            <Route path="/license" element={<div>LICENSE (legacy)</div>} />
            <Route path="/app/system/license" element={<div>LICENSE (canonical)</div>} />
          </Route>
          <Route path="/welcome" element={<div>WELCOME</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('FirstRunGate — license exemption (N-3)', () => {
  beforeEach(() => {
    useAgentsStore.setState({ loaded: true, loading: false, agents: [] });
  });

  it('does not bounce the new canonical /app/system/license to /welcome', () => {
    renderGateAt('/app/system/license');
    expect(screen.getByText('LICENSE (canonical)')).toBeInTheDocument();
    expect(screen.queryByText('WELCOME')).not.toBeInTheDocument();
  });

  it('does not bounce the legacy /license alias to /welcome either', () => {
    renderGateAt('/license');
    expect(screen.getByText('LICENSE (legacy)')).toBeInTheDocument();
    expect(screen.queryByText('WELCOME')).not.toBeInTheDocument();
  });
});
