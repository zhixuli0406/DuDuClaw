import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { OSPage } from './OSPage';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';

const emptyQuadrants = {
  correct_detection: 0,
  false_alarm: 0,
  missed_need: 0,
  non_response: 0,
  correct_silence: 0,
  unknown: 0,
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAgentsStore.setState({ agents: [], loading: false, loaded: false, error: null });
});

describe('OSPage', () => {
  it('renders the header and an empty-fleet state', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'os.status') {
        return Promise.resolve({ edition: 'personal', quota: { limit: 1, used: 0 }, agents: [] });
      }
      if (method === 'os.gate.recent') {
        return Promise.resolve({ recent: [], quadrants: emptyQuadrants });
      }
      if (method === 'os.events.recent') {
        return Promise.resolve({ events: [] });
      }
      if (method === 'agents.list') {
        return Promise.resolve({ agents: [] });
      }
      return Promise.resolve(null);
    });

    renderWithProviders(<OSPage />);

    expect(await screen.findByText('OS')).toBeInTheDocument();
    expect(await screen.findByText('No AI staff yet')).toBeInTheDocument();
    expect(await screen.findByText('No proactive-outreach records yet')).toBeInTheDocument();
    expect(await screen.findByText('No events yet')).toBeInTheDocument();
    // Doctor never auto-runs — the only expensive RPC is on-demand.
    expect(await screen.findByText('No check run yet')).toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('os.doctor.run');
  });

  it('renders an OS-native agent card, gate row, and event row', async () => {
    const nowIso = new Date().toISOString();
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list') {
        return Promise.resolve({ agents: [{ name: 'bruno', display_name: 'Bruno' }] });
      }
      if (method === 'os.status') {
        return Promise.resolve({
          edition: 'personal',
          quota: { limit: 1, used: 1 },
          agents: [
            {
              agent_id: 'bruno',
              os_native: true,
              watch: { paths: ['/Users/x/Downloads'], events: 12, dropped: 0 },
              frontmost: { poll_secs: 30, running: true },
              footprint: true,
              proactive: { enabled: true, base_threshold: 3, max_per_hour: 4 },
              induced_rules_count: 2,
            },
          ],
        });
      }
      if (method === 'os.gate.recent') {
        return Promise.resolve({
          recent: [
            {
              ts: nowIso,
              agent: 'bruno',
              event: 'os_file',
              score: 4,
              threshold: 3,
              interruptibility: 0.2,
              decision: 'allow',
              reason: 'downloads folder changed',
              latency_ms: 812,
              outcome: 'correct_detection',
            },
          ],
          quadrants: { ...emptyQuadrants, correct_detection: 1 },
        });
      }
      if (method === 'os.events.recent') {
        return Promise.resolve({
          events: [{ id: 1, event: 'os_file', ts: nowIso, source: 'internal_broadcast', payload: { path: '/x' } }],
        });
      }
      return Promise.resolve(null);
    });

    renderWithProviders(<OSPage />);

    expect(await screen.findByText('OS')).toBeInTheDocument();
    // Overview card (avatar name) + gate table row both key off displayName.
    expect((await screen.findAllByText('Bruno')).length).toBeGreaterThan(0);
    expect(await screen.findByText('Running')).toBeInTheDocument();
    expect(await screen.findByText('2 auto-induced rules')).toBeInTheDocument();
    expect(await screen.findByText('Reached out')).toBeInTheDocument();
    // "os_file" appears both in the gate decision table and the events table.
    expect((await screen.findAllByText('os_file')).length).toBeGreaterThan(0);
  });

  it('runs the environment doctor only when the button is clicked', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'os.status') {
        return Promise.resolve({ edition: 'enterprise', quota: { limit: null, used: 0 }, agents: [] });
      }
      if (method === 'os.gate.recent') {
        return Promise.resolve({ recent: [], quadrants: emptyQuadrants });
      }
      if (method === 'os.events.recent') {
        return Promise.resolve({ events: [] });
      }
      if (method === 'agents.list') {
        return Promise.resolve({ agents: [] });
      }
      if (method === 'os.doctor.run') {
        return Promise.resolve({
          checks: [{ id: 'notification', status: 'ok', detail: 'Test notification sent.' }],
        });
      }
      return Promise.resolve(null);
    });

    renderWithProviders(<OSPage />);

    expect(await screen.findByText('No check run yet')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Run check' }));

    expect(await screen.findByText('Test notification sent.')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('os.doctor.run');
  });

  // ── P4-3+ live event tail ──────────────────────────────────────────

  it('subscribes to the live os.events.entry tail and prepends pushed events ahead of the snapshot', async () => {
    const nowIso = new Date().toISOString();
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'os.status') {
        return Promise.resolve({ edition: 'personal', quota: { limit: 1, used: 0 }, agents: [] });
      }
      if (method === 'os.gate.recent') {
        return Promise.resolve({ recent: [], quadrants: emptyQuadrants });
      }
      if (method === 'os.events.recent') {
        return Promise.resolve({
          events: [
            { id: 1, event: 'os_file', ts: nowIso, source: 'internal_broadcast', payload: { path: '/old.pdf' } },
          ],
        });
      }
      if (method === 'os.events.subscribe') {
        return Promise.resolve({ success: true, subscribed: true });
      }
      if (method === 'agents.list') {
        return Promise.resolve({ agents: [] });
      }
      return Promise.resolve(null);
    });

    renderWithProviders(<OSPage />);

    // Snapshot row loads first.
    expect(await screen.findByText('os_file')).toBeInTheDocument();
    // The page opted this connection into the live tail.
    expect(mockWsClient.call).toHaveBeenCalledWith('os.events.subscribe');
    expect(await screen.findByText('Live')).toBeInTheDocument();

    // Grab the handler the page registered for the live tail and simulate a push.
    const registration = mockWsClient.subscribe.mock.calls.find(([event]) => event === 'os.events.entry');
    expect(registration).toBeTruthy();
    const pushHandler = registration![1] as (payload: unknown) => void;

    act(() => {
      pushHandler({
        event: 'os_frontmost',
        ts: new Date().toISOString(),
        source: 'internal_broadcast',
        payload: { app: 'Xcode' },
      });
    });

    // Pushed row renders alongside the snapshot row (prepended ahead of it).
    expect(await screen.findByText('os_frontmost')).toBeInTheDocument();
    expect(await screen.findByText('os_file')).toBeInTheDocument();
    const rows = await screen.findAllByRole('row');
    // Header row + pushed row + snapshot row; pushed row must come first in
    // document order (prepended, not appended).
    const bodyRowTexts = rows.slice(1).map((r) => r.textContent ?? '');
    const pushedIdx = bodyRowTexts.findIndex((t) => t.includes('os_frontmost'));
    const snapshotIdx = bodyRowTexts.findIndex((t) => t.includes('os_file'));
    expect(pushedIdx).toBeGreaterThanOrEqual(0);
    expect(snapshotIdx).toBeGreaterThanOrEqual(0);
    expect(pushedIdx).toBeLessThan(snapshotIdx);
  });

  it('shows a disconnected hint and keeps manual refresh working when the connection drops', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'os.status') {
        return Promise.resolve({ edition: 'personal', quota: { limit: 1, used: 0 }, agents: [] });
      }
      if (method === 'os.gate.recent') {
        return Promise.resolve({ recent: [], quadrants: emptyQuadrants });
      }
      if (method === 'os.events.recent') {
        return Promise.resolve({ events: [] });
      }
      if (method === 'agents.list') {
        return Promise.resolve({ agents: [] });
      }
      return Promise.resolve(null);
    });

    useConnectionStore.setState({ state: 'disconnected' as never, error: null });
    renderWithProviders(<OSPage />);

    expect(await screen.findByText('Disconnected')).toBeInTheDocument();
    // The manual refresh button stays present and enabled even while offline.
    const refreshButtons = screen.getAllByRole('button', { name: 'Refresh' });
    expect(refreshButtons.length).toBeGreaterThan(0);
    expect(mockWsClient.call).not.toHaveBeenCalledWith('os.events.subscribe');
  });
});
