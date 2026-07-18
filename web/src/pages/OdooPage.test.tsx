import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { OdooPage } from './OdooPage';

beforeEach(() => {
  vi.clearAllMocks();
  // All RPCs (odoo.status / odoo.config / agents.list) resolve through the same
  // mock — a shape that satisfies a disconnected status, an empty config and an
  // empty agent roster.
  mockWsClient.call.mockResolvedValue({ connected: false, agents: [] });
});

describe('OdooPage', () => {
  it('renders the connection settings section after load', async () => {
    renderWithProviders(<OdooPage />);

    expect(await screen.findByText('Connection Settings')).toBeInTheDocument();
  });

  it('renders the save action', async () => {
    renderWithProviders(<OdooPage />);

    expect(await screen.findByRole('button', { name: /^save$/i })).toBeInTheDocument();
  });
});
