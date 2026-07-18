import { describe, it, expect, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { SettingsPage } from './SettingsPage';

// Sections fire RPCs on mount (system.config, redaction.get, …); a broad
// resolved-empty mock keeps them quiet so we can smoke-test the shell + rail.
describe('SettingsPage', () => {
  beforeEach(() => {
    mockWsClient.call.mockResolvedValue({});
  });

  it('renders the settings rail with the General tab active by default', () => {
    renderWithProviders(<SettingsPage />);
    // "General" reads both as a rail item and the active panel's title.
    expect(screen.getAllByText('General').length).toBeGreaterThan(0);
    // A neighbouring rail item is present (advanced group).
    expect(screen.getByRole('tab', { name: /System/i })).toBeInTheDocument();
  });

  it('switches to the Account panel when its rail item is clicked', async () => {
    renderWithProviders(<SettingsPage />);
    fireEvent.click(screen.getByRole('tab', { name: /Account/i }));
    // The Account panel renders its title (settings.account.title).
    await waitFor(() => {
      expect(screen.getByText('Account & Password')).toBeInTheDocument();
    });
  });
});
