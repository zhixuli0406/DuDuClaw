import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({ api: { system: {
  config: () => configMock(),
  updateConfig: (fields: Record<string, unknown>) => updateConfigMock(fields),
} } }));
import { AdvancedGeneralPreferences } from './AdvancedGeneralPreferences';

beforeEach(() => {
  vi.clearAllMocks();
  configMock.mockResolvedValue({ config: '[redaction]\nlevel = "trace"\n[general]\nlog_level = "warn"\n[tick]\nstrategy = "aggressive"\n[rotation]\nstrategy = "round_robin"\n' });
  updateConfigMock.mockResolvedValue({ success: true, restart_required: [] });
});

describe('migrated general preferences', () => {
  it.each([
    ['logging', 'Log Level', 'Warnings'],
    ['rotation', 'Account Rotation Strategy', 'Round robin'],
  ] as const)('reads %s from its own section and sends no unchanged values', async (field, label, value) => {
    renderWithProviders(<AdvancedGeneralPreferences field={field} />);
    await waitFor(() => expect(screen.getByRole('combobox', { name: label })).toHaveTextContent(value));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(updateConfigMock).not.toHaveBeenCalled();
  });

  it('keeps saving disabled when loading fails', async () => {
    configMock.mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<AdvancedGeneralPreferences field="logging" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled());
    expect(updateConfigMock).not.toHaveBeenCalled();
  });
});
