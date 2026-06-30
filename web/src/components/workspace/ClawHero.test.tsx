import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ClawHero } from './ClawHero';
import { useUiModeStore } from '@/stores/ui-mode-store';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useUiModeStore.setState({ mode: 'workspace', chosen: true });
});

describe('ClawHero', () => {
  it('renders the title and value props, privacy first', () => {
    renderWithProviders(<ClawHero />);
    expect(screen.getByRole('heading', { name: /your first AI employee/i })).toBeInTheDocument();
    expect(screen.getByText(/privacy first/i)).toBeInTheDocument();
  });

  it('the primary CTA invokes onStart (focus the prompt)', async () => {
    const onStart = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<ClawHero onStart={onStart} />);
    await user.click(screen.getByRole('button', { name: /get started/i }));
    expect(onStart).toHaveBeenCalledTimes(1);
  });

  it('the manage CTA switches to dashboard mode', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ClawHero />);
    await user.click(screen.getByRole('button', { name: /manage agents/i }));
    expect(useUiModeStore.getState().mode).toBe('dashboard');
  });
});
