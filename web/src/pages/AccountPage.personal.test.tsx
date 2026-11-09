import { mockWsClient } from '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
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
