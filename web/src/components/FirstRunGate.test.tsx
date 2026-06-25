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
