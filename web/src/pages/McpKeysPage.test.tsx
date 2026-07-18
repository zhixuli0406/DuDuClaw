import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { McpKeysPage } from './McpKeysPage';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('McpKeysPage', () => {
  it('shows the empty state when no keys exist', async () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);

    expect(await screen.findByText('No MCP keys yet')).toBeInTheDocument();
  });

  it('renders the create-key action', () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);

    expect(screen.getByRole('button', { name: /create key/i })).toBeInTheDocument();
  });
});
