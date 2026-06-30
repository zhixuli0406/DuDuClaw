import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ConnectorChips } from './ConnectorChips';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('ConnectorChips', () => {
  it('renders nothing for non-admins (all connectors are admin-gated)', () => {
    useAuthStore.setState({ user: { display_name: 'E', role: 'employee' } as never });
    const { container } = renderWithProviders(<ConnectorChips />);
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the connectors control for admins', () => {
    useAuthStore.setState({ user: { display_name: 'A', role: 'admin' } as never });
    useSystemStore.setState({ status: { channels_connected: 2 } as never });
    renderWithProviders(<ConnectorChips />);
    expect(screen.getByRole('button', { name: /connectors/i })).toBeInTheDocument();
  });
});
