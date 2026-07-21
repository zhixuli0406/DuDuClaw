import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';

// SystemTab loads current values via system.config on mount and writes them back
// via system.update_config on save. Mock the api so we can assert the
// remote-access allowlist (allowed_origins) round-trips correctly.
const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (fields: Record<string, unknown>) => updateConfigMock(fields),
    },
  },
}));

import { SystemTab } from './SystemTab';

beforeEach(() => {
  // Two seeded origins so we can render chips and remove one.
  configMock.mockResolvedValue({
    config: 'bind = "127.0.0.1"\nport = 3100\n',
    allowed_origins: ['dash.example.com', 'box.tailnet.ts.net'],
  });
  updateConfigMock.mockResolvedValue({ success: true, changes: [], applied: true });
  // fetchAgents is called on mount; stub it out and keep the roster empty.
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [] as never });
});

describe('<SystemTab> remote-access allowlist', () => {
  it('renders the seeded allowed origins as chips', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => {
      expect(screen.getByText('dash.example.com')).toBeInTheDocument();
      expect(screen.getByText('box.tailnet.ts.net')).toBeInTheDocument();
    });
  });

  it('adds a new origin via the input + Add button', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    const input = screen.getByLabelText('Add a URL') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'new.example.com' } });
    fireEvent.click(screen.getByRole('button', { name: /Add/i }));

    await waitFor(() => expect(screen.getByText('new.example.com')).toBeInTheDocument());
    // Draft input is cleared after adding.
    expect(input.value).toBe('');
  });

  it('removes an origin when its X is clicked', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    fireEvent.click(screen.getByLabelText('Remove dash.example.com'));
    await waitFor(() => expect(screen.queryByText('dash.example.com')).not.toBeInTheDocument());
    // The other chip survives.
    expect(screen.getByText('box.tailnet.ts.net')).toBeInTheDocument();
  });

  it('sends the edited allowlist in the update_config payload', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    // Add one, remove one → payload should reflect the net set.
    const input = screen.getByLabelText('Add a URL');
    fireEvent.change(input, { target: { value: 'added.example.com' } });
    fireEvent.click(screen.getByRole('button', { name: /Add/i }));
    fireEvent.click(screen.getByLabelText('Remove box.tailnet.ts.net'));

    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    const payload = updateConfigMock.mock.calls[0][0] as { allowed_origins: string[] };
    expect(payload.allowed_origins).toEqual(['dash.example.com', 'added.example.com']);
  });
});
