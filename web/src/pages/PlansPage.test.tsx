import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { PlansPage } from './PlansPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ plans: [], agents: [], steps: [] });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('PlansPage', () => {
  it('renders the collection header with the plan icon and title', () => {
    renderWithProviders(<PlansPage />);
    expect(screen.getByRole('heading', { name: 'Shared Plans' })).toBeInTheDocument();
  });

  it('offers a create-plan action', () => {
    renderWithProviders(<PlansPage />);
    expect(screen.getAllByRole('button', { name: 'New plan' }).length).toBeGreaterThan(0);
  });
});
