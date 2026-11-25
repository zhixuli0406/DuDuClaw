import { describe, expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentSeatSummary } from './AgentSeatSummary';

describe('avatar seat display', () => {
  it('shows ordinary seats and exempt avatars separately', async () => {
    mockWsClient.call.mockResolvedValue({ seats_used: 5, seat_limit: 5, avatars_exempt: 2 });
    renderWithProviders(<AgentSeatSummary />);
    expect(await screen.findByText('AI employee seats: 5 / 5 · AI avatars: 2')).toBeInTheDocument();
    expect(screen.getByText('AI avatars do not use employee seats')).toBeInTheDocument();
  });
  it('does not invent counts when the license status is unavailable', async () => {
    mockWsClient.call.mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<AgentSeatSummary />);
    expect(await screen.findByText('AI avatars do not use employee seats')).toBeInTheDocument();
    expect(screen.queryByTestId('agent-seat-counts')).not.toBeInTheDocument();
    vi.clearAllMocks();
  });
});
