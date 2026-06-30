import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { LauncherCard } from './LauncherCard';
import { LAUNCHER_CARDS } from './launcher-model';
import { useUiModeStore } from '@/stores/ui-mode-store';

const tasks = LAUNCHER_CARDS.find((c) => c.id === 'tasks')!;
const claw = LAUNCHER_CARDS.find((c) => c.id === 'claw')!;
const slides = LAUNCHER_CARDS.find((c) => c.id === 'slides')!; // coming-soon

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useUiModeStore.setState({ mode: 'workspace', chosen: true });
});

describe('LauncherCard', () => {
  it('a dashboard-route card switches to dashboard mode on click', async () => {
    const user = userEvent.setup();
    renderWithProviders(<LauncherCard card={tasks} />);
    await user.click(screen.getByRole('button', { name: /task board/i }));
    expect(useUiModeStore.getState().mode).toBe('dashboard');
  });

  it('the /webchat card stays in workspace mode', async () => {
    const user = userEvent.setup();
    renderWithProviders(<LauncherCard card={claw} />);
    await user.click(screen.getByRole('button', { name: /claw chat/i }));
    expect(useUiModeStore.getState().mode).toBe('workspace');
  });

  it('a coming-soon card is disabled and inert', () => {
    renderWithProviders(<LauncherCard card={slides} />);
    expect(screen.getByRole('button', { name: /ai slides/i })).toBeDisabled();
  });
});
