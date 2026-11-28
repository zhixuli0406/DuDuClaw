import { resetCompanyTimeZone, useCompanyTimeStore } from '@/lib/company-time';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { LogsPage } from './LogsPage';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useLogsStore } from '@/stores/logs-store';

afterEach(() => resetCompanyTimeZone());

beforeEach(() => {
  resetCompanyTimeZone();
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAgentsStore.setState({ agents: [], loaded: false });
  useLogsStore.setState({ entries: [], paused: false, filter: { level: null, agentId: null, keyword: '' } });
  mockWsClient.call.mockResolvedValue({
    events: [],
    source_counts: { security: 0, tool_call: 0, channel_failure: 0, feedback: 0 },
  });
});

describe('LogsPage', () => {
  it('updates audit timestamps when the company zone loads', async () => {
    useCompanyTimeStore.setState({ status: 'set', zone: 'UTC' });
    mockWsClient.call.mockImplementation(async (method: string) => method === 'agents.list'
      ? { agents: [] }
      : { events: [{
        timestamp: '2026-11-27T01:30:00Z', source: 'security',
        event_type: 'company-zone-test', agent_id: 'worker', severity: 'info',
        summary: '', details: {},
      }] });
    renderWithProviders(<LogsPage />);
    expect(await screen.findByText(/01:30:00/)).toBeInTheDocument();

    act(() => useCompanyTimeStore.setState({ status: 'set', zone: 'Asia/Taipei' }));
    expect(await screen.findByText(/09:30:00/)).toBeInTheDocument();
    expect(screen.queryByText(/01:30:00/)).not.toBeInTheDocument();
  });

  it('renders the audit logs heading', () => {
    // Default tab is history — no realtime WS subscribe on load.
    renderWithProviders(<LogsPage />);
    expect(screen.getByText('Audit Logs')).toBeInTheDocument();
  });

  it.each([
    ['worker', 'Actual Worker'],
    ['Actual Worker', 'Actual Worker'],
    ['unknown-worker', 'unknown-worker'],
  ])('resolves the audit agent %s to a human name when known', async (agentId, label) => {
    const event = {
      timestamp: '2026-10-31T00:00:00Z', source: 'channel_failure',
      event_type: 'channel.AuthFailed', agent_id: agentId, severity: 'warning',
      summary: '{"origin":"Codex"}', details: { channel_failure: { agent: agentId } },
    };
    mockWsClient.call.mockImplementation(async (method: string) => method === 'agents.list'
      ? { agents: [{ name: 'worker', display_name: 'Actual Worker' }] }
      : { events: [event] });
    renderWithProviders(<LogsPage />);

    expect(await screen.findByText(`${label}'s work did not finish`)).toBeInTheDocument();
    expect(screen.getByText(label)).toBeInTheDocument();
    expect(event.agent_id).toBe(agentId);
  });

  it.each(['root', 'runtime'])('shows the protected policy source %s', async (source) => {
    mockWsClient.call.mockResolvedValue({ events: [{
      timestamp: '2026-11-05T00:00:00Z', source: 'security',
      event_type: source === 'root' ? 'engine_socket_policy_changed' : 'engine_socket_warning_started',
      agent_id: 'protected-linux', severity: 'warning', summary: '',
      details: { policy_event_source: source },
    }] });
    renderWithProviders(<LogsPage />);
    expect(await screen.findByText(source)).toBeInTheDocument();
  });

  it('resolves realtime agent labels without changing their stored ids', async () => {
    useLogsStore.setState({ entries: [{ timestamp: '2026-10-31T00:00:00Z', level: 'warn', target: 'channel_reply', agent_id: 'worker', message: 'Reply failed' }] });
    mockWsClient.call.mockImplementation(async (method: string) => method === 'agents.list'
      ? { agents: [{ name: 'worker', display_name: 'Actual Worker' }] }
      : {});
    renderAt('/manage/logs?tab=realtime');
    expect(await screen.findByText('Actual Worker')).toBeInTheDocument();
    expect(useLogsStore.getState().entries[0].agent_id).toBe('worker');
  });
});

// ── P11 (state-as-URL): tab + source + severity are deep-linkable ────────────

function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route
            path="/manage/logs"
            element={
              <>
                <LogsPage />
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

describe('LogsPage — state as URL (P11)', () => {
  it('opens on the tab named by ?tab= instead of always on history', async () => {
    renderAt('/manage/logs?tab=realtime');
    // The realtime tab is the only one that subscribes to the live log stream.
    await waitFor(() => expect(mockWsClient.subscribe).toHaveBeenCalled());
  });

  it('restores the source filter from ?source= on a deep link', async () => {
    renderAt('/manage/logs?source=security');
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'audit.unified_log',
        expect.objectContaining({ sources: ['security'] }),
      ),
    );
  });

  it('restores the severity filter from ?severity= on a deep link', async () => {
    renderAt('/manage/logs?severity=critical');
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'audit.unified_log',
        expect.objectContaining({ severity_filter: 'critical' }),
      ),
    );
  });

  it('writes a chip toggle back into the URL', async () => {
    const user = userEvent.setup();
    renderAt('/manage/logs');
    const chip = await screen.findByRole('button', { name: /Security/i });
    await user.click(chip);
    await waitFor(() =>
      expect(screen.getByTestId('search-probe')).toHaveTextContent('source=security'),
    );
  });

  it('ignores an unknown severity rather than filtering to nothing', async () => {
    renderAt('/manage/logs?severity=nonsense');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalled());
    const params = mockWsClient.call.mock.calls.find(
      (c: unknown[]) => c[0] === 'audit.unified_log',
    )?.[1] as Record<string, unknown> | undefined;
    expect(params).toBeDefined();
    expect(params).not.toHaveProperty('severity_filter');
  });
});

describe('S-10 Drive write history', () => {
  it('sends the quick filter and persists it in the URL', async () => {
    renderAt('/manage/logs');
    await userEvent.click(screen.getByRole('button', { name: 'Drive write' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('audit.unified_log', expect.objectContaining({ tool_name_filter: 'google_drive_write' })));
    expect(screen.getByTestId('search-probe')).toHaveTextContent('tool_name_filter=google_drive_write');
  });
  it('preselects the quick filter from a link', async () => {
    renderAt('/manage/logs?tool_name_filter=google_drive_write');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('audit.unified_log', expect.objectContaining({ tool_name_filter: 'google_drive_write' })));
    expect(screen.getByRole('button', { name: 'Drive write' })).toHaveAttribute('aria-pressed', 'true');
  });
  it('renders translated columns and resolves people without exposing raw records', async () => {
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'agents.list') return { agents: [{ name: 'agent-secret', display_name: 'Sales assistant' }] };
      if (method === 'users.list') return { users: [{ id: 'user-secret', display_name: 'Mei', email: 'mei@example.test' }] };
      return { events: [{ timestamp: '2026-11-27T01:30:00Z', source: 'tool_call', event_type: 'google_drive_write', agent_id: 'agent-secret', severity: 'warning', summary: 'raw-secret', details: { tool_call: {
        tool_name: 'google_drive_write', identity_kind: 'personal', account_email: 'mei@example.test', requester: { kind: 'dashboard_user', label: 'Mei' }, action: 'move', file_name: 'Proposal.pdf', folder_rule: 'folder-secret', approval: { mode: 'member_and_admin', approval_ids: ['approval-secret'], decided_by: ['dashboard:user-secret', 'admin@example.test'] }, outcome: 'refused', reason: 'folder_out_of_scope', post_check: 'none', file_id: 'file-secret', token: 'token-secret',
      } } }] };
    });
    renderAt('/manage/logs?tool_name_filter=google_drive_write');
    expect(await screen.findByText('Proposal.pdf')).toBeInTheDocument();
    expect(await screen.findAllByText(/Mei/)).not.toHaveLength(0);
    for (const label of ['Time', 'AI employee', 'Identity and account', 'Requester', 'Action', 'File name', 'Folder rule', 'Confirmation and decision makers', 'Result', 'Post-check']) expect(screen.getByRole('columnheader', { name: label })).toBeInTheDocument();
    for (const text of ['Personal connection', 'Move', 'Refused', 'The folder is outside the allowed list.']) expect(screen.getByText(text)).toBeInTheDocument();
    for (const raw of ['agent-secret', 'user-secret', 'folder-secret', 'approval-secret', 'file-secret', 'token-secret', 'folder_out_of_scope', 'raw-secret']) expect(document.body).not.toHaveTextContent(raw);
  });
});

it.each([
  ['system', '(Schedule)', '(Schedule)'],
  ['unknown', '', '(System)'],
  ['system', '   ', '(System)'],
  ['legacy', '<b>Legacy label</b>', '<b>Legacy label</b>'],
  ['channel_user', 'Channel member', 'Channel member'],
  ['channel_unbound', 'Unbound visitor', 'Unbound visitor'],
])('renders requester %s labels as text and handles empty system labels', async (kind, label, expected) => {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'agents.list') return { agents: [] };
    if (method === 'users.list') return { users: [] };
    if (method === 'google.drive_write.get') return { folders: [{ id: 'hidden-folder', name: 'Sales', kind: 'folder' }] };
    return { events: [{ timestamp: '2026-11-27T00:00:00Z', source: 'tool_call', event_type: 'google_drive_write', agent_id: 'hidden-agent', severity: 'info', details: { tool_call: { tool_name: 'google_drive_write', identity_kind: 'home_oauth', account_email: 'office@example.test', requester: { kind, label }, action: 'create', file_name: 'Note.txt', folder_rule: 'hidden-folder', approval: { mode: 'none', approval_ids: ['hidden-approval'], decided_by: [] }, outcome: 'confirmed', reason: 'unknown_reason_secret', post_check: 'ok' } } }] };
  });
  renderAt('/manage/logs');
  expect(await screen.findByText(expected)).toBeInTheDocument();
  expect(document.querySelector('b')).toBeNull();
  expect(await screen.findByText('Sales')).toBeInTheDocument();
  expect(screen.getByText('Company account')).toBeInTheDocument(); expect(screen.getByText('Location verified')).toBeInTheDocument();
  expect(screen.getByText('Incomplete')).toBeInTheDocument();
  for (const raw of ['hidden-agent', 'hidden-user', 'hidden-folder', 'hidden-approval', 'unknown_reason_secret']) expect(document.body).not.toHaveTextContent(raw);
});
