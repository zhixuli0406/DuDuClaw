import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { LogsPage } from './LogsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
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
});
