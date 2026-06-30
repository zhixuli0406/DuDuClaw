import { describe, it, expect, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ModeToggle } from './ModeToggle';
import { useUiModeStore } from '@/stores/ui-mode-store';

beforeEach(() => {
  localStorage.clear();
  useUiModeStore.setState({ mode: 'dashboard', chosen: false });
});

describe('ModeToggle', () => {
  it('reflects and updates the active mode', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ModeToggle />);

    const workspace = screen.getByRole('radio', { name: /workspace/i });
    const dashboard = screen.getByRole('radio', { name: /advanced/i });
    expect(dashboard).toHaveAttribute('aria-checked', 'true');
    expect(workspace).toHaveAttribute('aria-checked', 'false');

    await user.click(workspace);
    expect(useUiModeStore.getState().mode).toBe('workspace');
    expect(screen.getByRole('radio', { name: /workspace/i })).toHaveAttribute(
      'aria-checked',
      'true',
    );
  });
});
