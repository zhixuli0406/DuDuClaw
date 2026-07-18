import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { TimelinePage } from './TimelinePage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('TimelinePage', () => {
  it('renders the page header and range control', () => {
    renderWithProviders(<TimelinePage />);
    expect(screen.getByRole('heading', { name: 'Work timeline' })).toBeInTheDocument();
    // Segmented time-range control (radiogroup with 1h/6h/24h/7d).
    expect(screen.getByRole('radio', { name: '24 hours' })).toBeInTheDocument();
  });
});
