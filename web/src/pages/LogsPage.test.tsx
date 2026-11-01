import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
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

beforeEach(() => {
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
