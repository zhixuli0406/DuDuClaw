import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { RoutinesPage } from './RoutinesPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ tasks: [] });
});

describe('RoutinesPage', () => {
  it('renders the collection header with the create action', () => {
    renderWithProviders(<RoutinesPage />);
    expect(screen.getByRole('heading', { name: 'Routines' })).toBeInTheDocument();
    // Primary "new routine" CTA (routines.add).
    expect(screen.getAllByText('New routine').length).toBeGreaterThan(0);
  });
});
