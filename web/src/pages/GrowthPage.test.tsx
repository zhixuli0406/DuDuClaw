import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { GrowthPage } from './GrowthPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('GrowthPage', () => {
  it('renders the growth header', () => {
    renderWithProviders(<GrowthPage />);
    expect(screen.getByRole('heading', { name: 'Growth' })).toBeInTheDocument();
  });
});
