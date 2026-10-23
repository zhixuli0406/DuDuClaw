import { beforeEach, expect, it } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { BrowserLoginState } from './BrowserLoginState';

const SAVED = { enabled: false, saved: true, sealed_bytes: 1048576, updated_at: 1800000000, hosts_count: 2, can_reset: true };
function show() {
  return render(<IntlProvider locale="en" messages={en}><BrowserLoginState agentId="alice" /></IntlProvider>);
}
beforeEach(() => mockWsClient.call.mockReset());

it('shows retained saved state while persistence is disabled', async () => {
  mockWsClient.call.mockResolvedValue(SAVED);
  show();
  expect(await screen.findByText('Login state is saved.')).toBeInTheDocument();
  expect(screen.getByText(/Persistence is off/)).toBeInTheDocument();
  expect(screen.getByText(/1.0 MiB · 2 websites/)).toBeInTheDocument();
});

it('asks for confirmation before ending the browser and resetting', async () => {
  mockWsClient.call.mockResolvedValue(SAVED);
  show();
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Clear login state' }));
  expect(screen.getByRole('alert')).toHaveTextContent('This ends the current browser');
  expect(mockWsClient.call).not.toHaveBeenCalledWith('computer_profiles.reset', expect.anything());
  await user.click(screen.getByRole('button', { name: 'Clear login state' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('computer_profiles.reset', { agent_id: 'alice', confirm: true }));
});

it('does not show reset controls for a watch-only account', async () => {
  mockWsClient.call.mockResolvedValue({ ...SAVED, can_reset: false });
  show();
  await screen.findByText('Login state is saved.');
  expect(screen.queryByRole('button', { name: 'Clear login state' })).not.toBeInTheDocument();
});
