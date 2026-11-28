import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { formatDateTime } from '@/lib/company-time';
import en from '@/i18n/en.json';

const getMock = vi.fn();
const setMock = vi.fn();
const usersMock = vi.fn();
vi.mock('@/lib/api', () => ({ api: {
  adminAi: { get: () => getMock(), set: (params: unknown) => setMock(params) },
  users: { list: () => usersMock() },
} }));
import { AdminAiCard } from './AdminAiCard';

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin-id', role: 'admin' } as never });
  getMock.mockResolvedValue({ enabled: false });
  setMock.mockResolvedValue({ enabled: true, enabled_ack: { by: 'admin-id', at: '2026-11-27T14:03:00Z' } });
  usersMock.mockResolvedValue({ users: [{ id: 'admin-id', display_name: '王小明', email: 'admin@example.com' }] });
});

async function loaded() {
  await waitFor(() => expect(screen.getByRole('switch', { hidden: true })).not.toHaveAttribute('data-disabled'));
}

describe('AdminAiCard', () => {
  it('shows the off state with both complete lists', async () => {
    renderWithProviders(<AdminAiCard />);
    await loaded();
    expect(screen.getByRole('switch', { hidden: true })).not.toBeChecked();
    expect(screen.getAllByRole('list')).toHaveLength(2);
    expect(screen.getAllByRole('listitem')).toHaveLength(26);
    for (const key of Object.keys(en).filter((key) => /^settings.adminAi.(allowed|localOnly)\./.test(key))) {
      expect(screen.getByText(en[key as keyof typeof en])).toBeInTheDocument();
    }
    expect(screen.getByText(en['settings.adminAi.scheduleNote'])).toBeInTheDocument();
    expect(screen.getByText(en['settings.adminAi.auditNote'])).toBeInTheDocument();
  });

  it('requires acknowledgement before enabling and submits only the required fields', async () => {
    renderWithProviders(<AdminAiCard />);
    await loaded();
    fireEvent.click(screen.getByRole('switch', { hidden: true }));
    const dialog = screen.getByRole('dialog');
    const confirm = within(dialog).getByRole('button', { name: en['riskConfirm.confirm'] });
    expect(confirm).toBeDisabled();
    expect(screen.getByRole('switch', { hidden: true })).not.toBeChecked();
    fireEvent.click(confirm);
    expect(setMock).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole('checkbox'));
    fireEvent.click(confirm);
    await waitFor(() => expect(setMock).toHaveBeenCalledWith({ enabled: true, acknowledge_risk: true }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(screen.getByRole('switch', { hidden: true })).toBeChecked();
  });

  it('cancels enabling without changing the setting', async () => {
    renderWithProviders(<AdminAiCard />);
    await loaded();
    fireEvent.click(screen.getByRole('switch', { hidden: true }));
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('switch', { hidden: true })).not.toBeChecked();
    expect(setMock).not.toHaveBeenCalled();
  });

  it('disables immediately without acknowledgement or a dialog', async () => {
    getMock.mockResolvedValue({ enabled: true });
    setMock.mockResolvedValue({ enabled: false });
    renderWithProviders(<AdminAiCard />);
    await loaded();
    fireEvent.click(screen.getByRole('switch', { hidden: true }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    await waitFor(() => expect(setMock).toHaveBeenCalledWith({ enabled: false }));
    expect(screen.getByRole('switch', { hidden: true })).not.toBeChecked();
  });

  it('shows the enabling user name from users.list and the formatted time', async () => {
    getMock.mockResolvedValue({ enabled: true, enabled_ack: { by: 'admin-id', at: '2026-11-27T14:03:00Z' } });
    const { container } = renderWithProviders(<AdminAiCard />);
    expect(await screen.findByText(/Last enabled by 王小明/)).toHaveTextContent(formatDateTime('2026-11-27T14:03:00Z', { locale: 'en' }));
    expect(usersMock).toHaveBeenCalledTimes(1);
    expect(container).not.toHaveTextContent('admin-id');
  });

  it('disables the switch during loading and offers retry after a load error', async () => {
    getMock.mockRejectedValueOnce(new Error('load failed'));
    renderWithProviders(<AdminAiCard />);
    expect(screen.getByRole('switch', { hidden: true })).toHaveAttribute('data-disabled');
    await screen.findByRole('alert');
    expect(screen.getByRole('switch', { hidden: true })).toHaveAttribute('data-disabled');
    fireEvent.click(screen.getByRole('button', { name: en['errorState.retry'] }));
    await loaded();
    expect(getMock).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it.each([true, false])('keeps the prior setting and shows a refusal when setting enabled=%s fails', async (enabled) => {
    const refusal = '目前無法儲存，請稍後再試。';
    getMock.mockResolvedValue({ enabled: !enabled });
    setMock.mockRejectedValueOnce(new Error(refusal));
    renderWithProviders(<AdminAiCard />);
    await loaded();
    fireEvent.click(screen.getByRole('switch', { hidden: true }));
    if (enabled) {
      const dialog = screen.getByRole('dialog');
      fireEvent.click(within(dialog).getByRole('checkbox'));
      fireEvent.click(within(dialog).getByRole('button', { name: en['riskConfirm.confirm'] }));
    }
    expect(await screen.findByRole('alert')).toHaveTextContent(refusal);
    if (enabled) expect(within(screen.getByRole('dialog')).getByRole('alert')).toHaveTextContent(refusal);
    expect(screen.getByRole('switch', { hidden: true })).toHaveAttribute('aria-checked', String(!enabled));
  });

  it('does not load or render for a non-admin', () => {
    useAuthStore.setState({ user: { role: 'manager' } as never });
    const { container } = renderWithProviders(<AdminAiCard />);
    expect(container).toBeEmptyDOMElement();
    expect(getMock).not.toHaveBeenCalled();
    expect(usersMock).not.toHaveBeenCalled();
  });
});

it.each(['unreadable', 'malformed'])('explains invalid config %s as off and allows re-enabling', async config_state => {
  getMock.mockResolvedValue({ enabled: false, config_state });
  renderWithProviders(<AdminAiCard />);
  await loaded();
  expect(screen.getByText('The current settings cannot be read. The system treats this as off. Enabling it again will replace them with valid settings.')).toBeInTheDocument();
  expect(screen.getByRole('switch', { hidden: true })).not.toBeChecked();
  fireEvent.click(screen.getByRole('switch', { hidden: true }));
  expect(screen.getByRole('dialog')).toBeInTheDocument();
});

it('prefers the acknowledgement display name supplied by the server', async () => {
  getMock.mockResolvedValue({ enabled: true, config_state: 'ok', enabled_ack: { by: 'admin-id', by_name: 'Server Name', at: '2026-11-27T14:03:00Z' } });
  renderWithProviders(<AdminAiCard />);
  expect(await screen.findByText(/Last enabled by Server Name/)).toBeInTheDocument();
  expect(screen.queryByText(/Last enabled by 王小明/)).not.toBeInTheDocument();
  expect(screen.queryByText(/The current settings cannot be read/)).not.toBeInTheDocument();
});
