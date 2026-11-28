import { StrictMode } from 'react';
import { memberGoogleSignin } from '@/lib/google-signin';
import { mockWsClient } from '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { useSystemStore } from '@/stores/system-store';
import type { SystemStatus } from '@/lib/api';
import { AccountPage } from './AccountPage';

vi.mock('@/components/settings/sections/AccountTab', () => ({ AccountTab: () => <div>account-security-probe</div> }));
vi.mock('./account/MyPersonalEmployeesTab', () => ({
  MY_PERSONAL_TAB: { id: 'personal-employees', newIn: '1.81.0', labelId: 'myPersonal.tab' },
  MyPersonalEmployeesTab: () => <div>personal-employees-probe</div>,
}));

const status = (protectedMode: boolean) => ({
  version: '1.81.0',
  isolation_readiness: protectedMode ? { required: true, ready: true, components: {} } : null,
}) as unknown as SystemStatus;

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue(null);
});

describe('AccountPage — 我的個人 AI 員工 tab (v1.81)', () => {
  it('is unchanged outside protected mode', () => {
    useSystemStore.setState({ status: status(false) });
    renderWithProviders(<AccountPage />);
    expect(screen.getByText('account-security-probe')).toBeInTheDocument();
    expect(screen.queryByRole('tab')).toBeNull();
  });

  it('adds the tab, marked new in 1.81, in protected mode', async () => {
    useSystemStore.setState({ status: status(true) });
    const user = userEvent.setup();
    renderWithProviders(<AccountPage />);
    const tab = screen.getByRole('tab', { name: /My personal AI employees/ });
    expect(tab).toHaveTextContent('New');
    expect(screen.getByText('account-security-probe')).toBeInTheDocument();
    await user.click(tab);
    expect(await screen.findByText('personal-employees-probe')).toBeInTheDocument();
  });
});

it('strips the reauth ticket and completes once under StrictMode', async () => {
  window.history.replaceState({}, '', '/me/account?keep=1#gsi_reauth=ticket_123');
  const complete = vi.spyOn(memberGoogleSignin, 'reauthComplete').mockResolvedValue({});
  renderWithProviders(<StrictMode><AccountPage /></StrictMode>);
  expect(window.location.hash).toBe('');
  expect(window.location.search).toBe('?keep=1');
  expect(await screen.findByRole('status')).toHaveTextContent('Re-authenticated with your company Google account');
  expect(complete).toHaveBeenCalledExactlyOnceWith('ticket_123');
  complete.mockRestore(); window.history.replaceState({}, '', '/');
});

it('shows a reauth completion refusal and clears the ticket', async () => {
  window.history.replaceState({}, '', '/me/account#gsi_reauth=ticket_456');
  const message = 'Google 未提供近期身分驗證紀錄，請聯絡管理員重設兩步驟驗證。';
  const complete = vi.spyOn(memberGoogleSignin, 'reauthComplete').mockRejectedValue({ code: 'signin_reauth_unavailable', message });
  renderWithProviders(<AccountPage />);
  await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent(message));
  expect(window.location.hash).toBe('');
  complete.mockRestore(); window.history.replaceState({}, '', '/');
});
