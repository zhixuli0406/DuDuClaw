import { describe, it, expect, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
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

// Both editions use this same settings rail; the invitation tab is Admin-only.
it('shows the invitation tab with the 1.80.0 badge and opens its editor', async () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  useSystemStore.setState({ status: { version: '1.80.0' } as never });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'invitation.get') return { locale: 'en', content: { opening: 'Welcome', steps: 'Set a password', examples: ['A', 'B', 'C'], course_url: '' } };
    if (method === 'invitation.preview') return { subject: 'Welcome', html: '<p>Welcome</p>', text: 'Welcome', examples: ['A', 'B', 'C'] };
    return {};
  });
  renderWithProviders(<SettingsPage />);
  const tab = screen.getByRole('tab', { name: /Invitation email/i });
  expect(tab).toHaveTextContent('New');
  fireEvent.click(tab);
  expect(await screen.findByLabelText('Opening text')).toHaveValue('Welcome');
  expect(mockWsClient.call).toHaveBeenCalledWith('invitation.get', { locale: 'en' });
});

it('hides invitation settings from a manager', () => {
  useAuthStore.setState({ user: { id: 'manager', role: 'manager' } as never });
  mockWsClient.call.mockResolvedValue({});
  renderWithProviders(<SettingsPage />);
  expect(screen.queryByRole('tab', { name: /Invitation email/i })).not.toBeInTheDocument();
});
