import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, act } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { RoutinesPage } from './RoutinesPage';

/** Handlers registered via `client.subscribe`, keyed by event name — lets a
 *  test fire a server push the way the gateway would. */
const wsHandlers = new Map<string, (payload: unknown) => void>();

beforeEach(() => {
  vi.clearAllMocks();
  wsHandlers.clear();
  mockWsClient.call.mockResolvedValue({ tasks: [] });
  mockWsClient.subscribe.mockImplementation((event: string, handler: (p: unknown) => void) => {
    wsHandlers.set(event, handler);
    return () => wsHandlers.delete(event);
  });
  useConnectionStore.setState({ state: 'disconnected' });
});

describe('RoutinesPage', () => {
  it('renders the collection header with the create action', () => {
    renderWithProviders(<RoutinesPage />);
    expect(screen.getByRole('heading', { name: 'Routines' })).toBeInTheDocument();
    // Primary "new routine" CTA (routines.add).
    expect(screen.getAllByText('New routine').length).toBeGreaterThan(0);
  });

  // ── WP6: channel-action → dashboard live feedback ──────────────────────

  it('subscribes to cron.changed once authenticated', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    renderWithProviders(<RoutinesPage />);
    await waitFor(() => expect(wsHandlers.has('cron.changed')).toBe(true));
  });

  it('refetches the routine list when a cron.changed push arrives', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    renderWithProviders(<RoutinesPage />);

    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'cron.list')).toBe(true),
    );
    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;

    // A routine created from Telegram: the gateway pushes `cron.changed`.
    wsHandlers.get('cron.changed')?.({ action: 'created', name: '每日晨報' });

    await waitFor(
      () => {
        const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;
        expect(after).toBeGreaterThan(before);
      },
      { timeout: 2000 },
    );
  });

  // M4 — a bulk cron edit raises one event per row; the page must not fire one
  // RPC per event.
  it('collapses a burst of cron.changed pushes into a single refetch', async () => {
    vi.useFakeTimers();
    try {
      useConnectionStore.setState({ state: 'authenticated' });
      renderWithProviders(<RoutinesPage />);
      await vi.waitFor(() =>
        expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'cron.list')).toBe(true),
      );
      const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;

      const push = wsHandlers.get('cron.changed');
      expect(push).toBeDefined();
      for (let i = 0; i < 6; i++) push?.({ action: 'updated', id: `t${i}` });

      // Nothing yet — the trailing window is still open.
      expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length).toBe(before);

      await vi.advanceTimersByTimeAsync(500);
      expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length).toBe(
        before + 1,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  // M3 — a reconnect must re-read: pushes raised while the socket was down
  // are gone, so without this the list is stranded on pre-outage state.
  it('refetches after the socket reconnects', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    renderWithProviders(<RoutinesPage />);
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'cron.list')).toBe(true),
    );
    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;

    // Two separate renders — batching them into one would keep the effect
    // dependency at 'authenticated' and the test would prove nothing.
    await act(async () => {
      useConnectionStore.setState({ state: 'disconnected' });
    });
    await act(async () => {
      useConnectionStore.setState({ state: 'authenticated' });
    });

    await waitFor(() => {
      const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;
      expect(after).toBeGreaterThan(before);
    });
  });

  it('does not subscribe while the socket is unauthenticated', () => {
    renderWithProviders(<RoutinesPage />);
    expect(wsHandlers.has('cron.changed')).toBe(false);
  });
});
