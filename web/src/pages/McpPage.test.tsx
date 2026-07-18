import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { McpPage } from './McpPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // Empty MCP config: mcp.list → { agents: [], catalog: [] }; oauth providers → [].
  mockWsClient.call.mockResolvedValue({ agents: [], catalog: [], providers: [] });
});

describe('McpPage', () => {
  it('renders the slim header description', async () => {
    renderWithProviders(<McpPage />);
    expect(await screen.findByText('MCP Server Management')).toBeInTheDocument();
  });

  it('shows the empty state when no agents are configured', async () => {
    renderWithProviders(<McpPage />);
    expect(
      await screen.findByText('No MCP servers configured yet'),
    ).toBeInTheDocument();
  });

  it('exposes the primary import action button', () => {
    renderWithProviders(<McpPage />);
    expect(
      screen.getByRole('button', { name: /import from url/i }),
    ).toBeInTheDocument();
  });
});
