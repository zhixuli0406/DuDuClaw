import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { ComputerSessionPanel } from './ComputerSessionPanel';

// The real viewer loads noVNC; the panel tests only need to see it mount.
vi.mock('./ComputerViewer', () => ({
  ComputerViewer: ({ ticket }: { ticket: { input: boolean } }) => (
    <div data-testid="viewer">{ticket.input ? 'viewer-control' : 'viewer-view'}</div>
  ),
}));

const RUNNING = {
  ok: true,
  active: true,
  session_id: 'cu-1',
  state: 'running',
  actions_used: 3,
  max_actions: 50,
  seconds_left: 540,
  keep_alive_minutes: 30,
  paused_seconds_left: null,
  takeover: null,
  hold: null,
  viewers: 0,
  stream_available: true,
};

const TICKET = {
  ok: true,
  session_id: 'cu-1',
  ticket: '0123456789abcdef0123456789abcdef',
  password: 'Abcd2345',
  path: '/ws/computer-view',
  mode: 'viewonly',
  input: false,
  expires_in: 30,
};

function respond(map: Record<string, unknown>) {
  mockWsClient.call.mockImplementation(async (method: string) => {
    const v = map[method];
    if (v instanceof Error) throw v;
    return v ?? { ok: true };
  });
}

function renderPanel() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <ComputerSessionPanel agentId="alice" />
    </IntlProvider>,
  );
}

beforeEach(() => {
  mockWsClient.call.mockReset();
});

describe('ComputerSessionPanel', () => {
  it('shows the empty state when no session runs', async () => {
    respond({ 'computer_sessions.status': { ok: true, active: false } });
    renderPanel();
    expect(await screen.findByText(/has no computer-use session/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Take over/ })).not.toBeInTheDocument();
  });

  it('tells a viewer without authority so instead of showing controls', async () => {
    respond({ 'computer_sessions.status': new Error('permission denied') });
    renderPanel();
    expect(await screen.findByText(/cannot view this employee's computer/)).toBeInTheDocument();
  });

  it('shows status facts and opens a view-only viewer on Watch', async () => {
    respond({ 'computer_sessions.status': RUNNING, 'computer_sessions.view': TICKET });
    renderPanel();
    expect(await screen.findByText('Running')).toBeInTheDocument();
    expect(screen.getByText('3 / 50')).toBeInTheDocument();
    expect(screen.getByText('9:00')).toBeInTheDocument();
    expect(screen.getByText('30 min')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Watch/ }));
    expect(await screen.findByText('viewer-view')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.view', { agent_id: 'alice' });
  });

  it('takes over, then hands back with a note', async () => {
    respond({
      'computer_sessions.status': RUNNING,
      'computer_sessions.takeover': { ...TICKET, mode: 'control', input: true },
    });
    renderPanel();
    await userEvent.click(await screen.findByRole('button', { name: /Take over/ }));
    expect(await screen.findByText('viewer-control')).toBeInTheDocument();
    respond({
      'computer_sessions.status': {
        ...RUNNING,
        takeover: { holder: 'me@example.com', mine: true, idle_seconds_left: 600 },
      },
    });
    // The next poll shows the lease; force it by re-rendering through a status call.
    await waitFor(
      () => expect(screen.getByRole('button', { name: /Hand back/ })).toBeInTheDocument(),
      { timeout: 7000 },
    );
    await userEvent.type(screen.getByLabelText(/Note for the employee/), 'logged in');
    await userEvent.click(screen.getByRole('button', { name: /Hand back/ }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.hand_back', {
        agent_id: 'alice',
        note: 'logged in',
      }),
    );
  }, 15000);

  it('shows the injection hold with its categories and resumes', async () => {
    respond({
      'computer_sessions.status': {
        ...RUNNING,
        hold: { reason: 'injection_suspected', categories: ['instruction_override'], since: 1 },
      },
    });
    renderPanel();
    expect(await screen.findByRole('alert')).toHaveTextContent('instruction_override');
    expect(screen.getByText('Held for review')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Resume/ }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.resume', { agent_id: 'alice' }),
    );
  });

  it('asks before stopping and names who else holds control', async () => {
    respond({
      'computer_sessions.status': {
        ...RUNNING,
        state: 'paused',
        paused_seconds_left: 120,
        takeover: { holder: 'ops@example.com', mine: false, idle_seconds_left: 300 },
      },
    });
    renderPanel();
    expect(await screen.findByText(/ops@example.com has control/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Take over/ })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Stop session/ }));
    expect(mockWsClient.call).not.toHaveBeenCalledWith('computer_sessions.stop', expect.anything());
    await userEvent.click(screen.getByRole('button', { name: 'End it' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.stop', { agent_id: 'alice' }),
    );
  });
});
