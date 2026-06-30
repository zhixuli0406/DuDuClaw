import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { Sidebar } from './Sidebar';
import { useUiModeStore } from '@/stores/ui-mode-store';
import { useAuthStore } from '@/stores/auth-store';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useAuthStore.setState({ user: { display_name: 'A', role: 'admin' } as never });
});

describe('Sidebar shell modes', () => {
  it('workspace mode renders the narrow rail (Home), not the full group nav', () => {
    useUiModeStore.setState({ mode: 'workspace', chosen: true });
    renderWithProviders(<Sidebar />);
    // Rail items expose their label as an accessible name (sr-only / title).
    expect(screen.getByRole('link', { name: /home/i })).toBeInTheDocument();
    // Full-nav group headers are absent in workspace mode.
    expect(screen.queryByText(/^Overview$/)).not.toBeInTheDocument();
  });

  it('dashboard mode renders the full grouped navigation', () => {
    useUiModeStore.setState({ mode: 'dashboard', chosen: true });
    renderWithProviders(<Sidebar />);
    expect(screen.getByText(/^Overview$/)).toBeInTheDocument();
  });
});
