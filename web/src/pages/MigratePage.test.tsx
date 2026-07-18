import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { MigratePage } from './MigratePage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // Platform step renders without any API call; keep a benign default.
  mockWsClient.call.mockResolvedValue({});
});

describe('MigratePage', () => {
  it('renders the migration wizard heading', () => {
    renderWithProviders(<MigratePage />);
    expect(screen.getByText('Migrate your data')).toBeInTheDocument();
  });
});
