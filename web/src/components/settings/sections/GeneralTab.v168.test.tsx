import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (f: Record<string, unknown>) => updateConfigMock(f),
      autostartStatus: () => Promise.resolve({ supported: false, enabled: false }),
    },
  },
}));

import { GeneralTab } from './GeneralTab';

beforeEach(() => {
  vi.clearAllMocks();
  updateConfigMock.mockResolvedValue({ success: true, changes: [], restart_required: [] });
});

describe('<GeneralTab> v1.68', () => {
  it('reads log_level and strategy from their own sections, not look-alike keys', async () => {
    // `[redaction] level` and `[tick] strategy` used to win the unanchored regex.
    configMock.mockResolvedValue({
      config: '[redaction]\nlevel = "trace"\n\n[general]\nlog_level = "warn"\n\n[tick]\nstrategy = "aggressive"\n\n[rotation]\nstrategy = "round_robin"\n',
    });
    renderWithProviders(<GeneralTab />);
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Log Level' })).toHaveTextContent('Warnings'));
    // Saving without edits sends nothing — proving the loaded values match.
    fireEvent.click(await screen.findByRole('button', { name: 'Save' }));
    await new Promise((r) => setTimeout(r, 20));
    expect(updateConfigMock).not.toHaveBeenCalled();
  });
});
