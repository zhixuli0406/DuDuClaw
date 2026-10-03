import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SecurityPage } from './SecurityPage';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';

// v1.68 (W2): kill-switch triggers are armed explicitly; a save sends only
// the triggers the operator changed.
const RESPONSE = {
  events: [],
  triggers: { max_replies_per_minute: 30, max_consecutive_errors: 5, error_rate_threshold: 0.5, cost_limit_usd: 10 },
  triggers_enforced: { max_replies_per_minute: false, max_consecutive_errors: true, error_rate_threshold: false, cost_limit_usd: false },
  circuit_breaker: {
    frequency_window_secs: 60, frequency_max_replies: 20, similarity_threshold: 0.8,
    token_explosion_multiplier: 5, cooldown_secs: 30, half_open_allow_count: 3,
  },
  safety_words: { stop: ['stop'], stop_all: [], resume: [], status: [] },
  defensive_prompt: { enabled: true, languages: ['zh-TW'] },
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useSystemStore.setState({ status: null } as never);
  localStorage.clear();
  mockWsClient.call.mockResolvedValue(RESPONSE);
});

describe('SecurityPage kill-switch triggers', () => {
  it('shows enforced state and arms only the cost cap', async () => {
    renderWithProviders(<SecurityPage />);
    const errors = (await screen.findAllByTestId('trigger-max_consecutive_errors'))[0];
    expect(within(errors).getByText('In effect')).toBeInTheDocument();
    const cost = screen.getAllByTestId('trigger-cost_limit_usd')[0];
    expect(within(cost).getByText('Not set')).toBeInTheDocument();
    fireEvent.click(within(cost).getByRole('checkbox'));
    fireEvent.change(within(cost).getByRole('spinbutton'), { target: { value: '25' } });
    // Save → confirm dialog → confirm.
    fireEvent.click(screen.getAllByRole('button', { name: 'Save' })[0]);
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some(([m]) => m === 'killswitch.update')).toBe(true),
    );
    const params = mockWsClient.call.mock.calls.find(([m]) => m === 'killswitch.update')?.[1] as Record<string, unknown>;
    expect(params.triggers).toEqual({ cost_limit_usd: 25 });
  });
});
