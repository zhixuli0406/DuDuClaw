import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { mockWsClient } from '@/test/mocks';
import { api, type MyOdooStatus } from '@/lib/api';
import { formatDateTime } from '@/lib/company-time';
import messages from '@/i18n/zh-TW.json';
import { OdooConnectionCard } from './OdooConnectionCard';

const status: MyOdooStatus = {
  member_credentials_enabled: true, connected: false, state: null,
  login: null, odoo_name: null, server: 'https://odoo.example.test', database: 'internal-db',
  verified_at: null, last_used_at: null,
};
const connected: MyOdooStatus = {
  ...status, connected: true, state: 'ok', login: 'mei@example.test', odoo_name: '林美玲',
  verified_at: '2026-11-27T10:41:00Z', last_used_at: '2026-11-27T14:02:00Z',
};
function mount() {
  return render(<IntlProvider locale="zh-TW" messages={messages}><OdooConnectionCard /></IntlProvider>);
}
async function fill() {
  fireEvent.change(await screen.findByLabelText('Odoo 登入名'), { target: { value: 'mei@example.test' } });
  fireEvent.change(screen.getByLabelText('API 金鑰'), { target: { value: 'secret-odoo-key' } });
}
function submit() { fireEvent.click(screen.getByRole('button', { name: '連接 Odoo' })); }
beforeEach(() => {
  vi.restoreAllMocks();
  mockWsClient.call.mockReset();
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.odoo.connect' || method === 'me.odoo.test') return connected;
    if (method === 'me.odoo.disconnect') return { disconnected: true };
    return status;
  });
});

it('shows only administrator configuration when the switch is off', async () => {
  vi.spyOn(api.myOdoo, 'status').mockResolvedValue({ ...connected, member_credentials_enabled: false });
  mount();
  expect(await screen.findByText('由管理員設定')).toBeInTheDocument();
  expect(screen.getByText('公司目前由管理員設定 AI 員工使用的 Odoo 帳號，同仁不需要自己填。')).toBeInTheDocument();
  expect(screen.queryByLabelText('API 金鑰')).not.toBeInTheDocument();
  expect(screen.queryByRole('button')).not.toBeInTheDocument();
  expect(screen.queryByText(connected.login!)).not.toBeInTheDocument();
});
it('shows the form and company server without exposing the database', async () => {
  mount();
  expect(await screen.findByLabelText('Odoo 登入名')).toBeInTheDocument();
  expect(screen.getByLabelText('API 金鑰')).toHaveAttribute('type', 'password');
  expect(screen.getByLabelText('API 金鑰')).toHaveAttribute('autocomplete', 'off');
  expect(screen.getByText('只會連到公司設定的 https://odoo.example.test。')).toBeInTheDocument();
  expect(screen.queryByText('internal-db')).not.toBeInTheDocument();
});
it('keeps connect disabled until status succeeds', async () => {
  let resolve!: (value: MyOdooStatus) => void;
  vi.spyOn(api.myOdoo, 'status').mockReturnValue(new Promise(done => { resolve = done; }));
  mount();
  expect(screen.getByRole('button', { name: '連接 Odoo' })).toBeDisabled();
  submit();
  expect(mockWsClient.call).not.toHaveBeenCalledWith('me.odoo.connect', expect.anything());
  resolve(status);
  await fill();
  expect(screen.getByRole('button', { name: '連接 Odoo' })).toBeEnabled();
  submit();
  expect(await screen.findByText('已連接')).toBeInTheDocument();
});
it('does not allow connecting after a failed status and lets the member reload', async () => {
  const load = vi.spyOn(api.myOdoo, 'status').mockRejectedValueOnce('請使用瀏覽器重新登入後管理自己的 Odoo 連線。').mockResolvedValue(status);
  mount();
  expect(await screen.findByRole('alert')).toHaveTextContent('請使用瀏覽器重新登入後管理自己的 Odoo 連線。');
  expect(screen.getByRole('button', { name: '連接 Odoo' })).toBeDisabled();
  fireEvent.click(screen.getByRole('button', { name: '重新載入' }));
  await fill();
  expect(load).toHaveBeenCalledTimes(2);
});
it('shows connected details with formatted times and reverifies using the test RPC', async () => {
  vi.spyOn(api.myOdoo, 'status').mockResolvedValue(connected);
  mount();
  expect(await screen.findByText('已連接')).toBeInTheDocument();
  for (const text of [connected.login!, connected.odoo_name!, connected.server!,
    formatDateTime(connected.verified_at, { locale: 'zh-TW' }), formatDateTime(connected.last_used_at, { locale: 'zh-TW' })]) {
    expect(screen.getByText(text)).toBeInTheDocument();
  }
  fireEvent.click(screen.getByRole('button', { name: '重新驗證' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.odoo.test', {}));
});
it('shows the changed server and lets the member reverify', async () => {
  vi.spyOn(api.myOdoo, 'status').mockResolvedValue({ ...connected, state: 'needs_reverify', server: 'https://erp.example.test' });
  mount();
  expect(await screen.findByText('公司的 Odoo 位址已改成 https://erp.example.test。重新驗證之前，AI 員工不會用你的帳號。')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '重新驗證' }));
  expect(await screen.findByText('已連接')).toBeInTheDocument();
});
it('shows invalid credentials and reopens the form with no key', async () => {
  vi.spyOn(api.myOdoo, 'status').mockResolvedValue({ ...connected, state: 'invalid' });
  mount();
  expect(await screen.findByText('Odoo 不接受這組帳號與金鑰，可能是金鑰被刪除或密碼改了。')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '重新連接' }));
  expect(screen.getByLabelText('Odoo 登入名')).toHaveValue(connected.login);
  expect(screen.getByLabelText('API 金鑰')).toHaveValue('');
});
it.each([true, false])('sends only login and api_key and clears the key on success=%s', async success => {
  let finish!: () => void;
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.odoo.connect') return new Promise((resolve, reject) => {
      finish = () => success ? resolve(connected) : reject('Odoo 登入名稱和金鑰對不上，請確認後再試。');
    });
    return status;
  });
  const { container } = mount();
  await fill();
  expect(container.textContent).not.toContain('secret-odoo-key');
  submit();
  expect(mockWsClient.call).toHaveBeenCalledWith('me.odoo.status', {});
  expect(mockWsClient.call).toHaveBeenCalledWith('me.odoo.connect', { login: 'mei@example.test', api_key: 'secret-odoo-key' });
  expect(screen.getByLabelText('API 金鑰')).toHaveValue('');
  expect(container.innerHTML).not.toContain('secret-odoo-key');
  finish();
  if (success) {
    expect(await screen.findByText('已連接')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '斷開' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '斷開' }));
    expect(await screen.findByLabelText('API 金鑰')).toHaveValue('');
  } else {
    expect(await screen.findByRole('alert')).toHaveTextContent('Odoo 登入名稱和金鑰對不上，請確認後再試。');
    expect(screen.getByLabelText('API 金鑰')).toHaveValue('');
    expect(screen.getByText('只會連到公司設定的 https://odoo.example.test。')).toBeInTheDocument();
  }
  expect(container.innerHTML).not.toContain('secret-odoo-key');
});
it('refreshes the server confirmation after a refusal and asks for a manual retry', async () => {
  const load = vi.spyOn(api.myOdoo, 'status').mockResolvedValueOnce(status).mockResolvedValue({ ...status, server: 'https://new.example.test' });
  vi.spyOn(api.myOdoo, 'connect').mockRejectedValue('請先重新整理連線頁面，確認 Odoo 伺服器位址和資料庫後再連線或重新驗證。');
  mount(); await fill(); submit();
  expect(await screen.findByText('只會連到公司設定的 https://new.example.test。')).toBeInTheDocument();
  expect(screen.getByRole('alert')).toHaveTextContent('請確認目前顯示的伺服器位址，再試一次。');
  expect(load).toHaveBeenCalledTimes(2);
  expect(api.myOdoo.connect).toHaveBeenCalledTimes(1);
  expect(screen.getByLabelText('API 金鑰')).toHaveValue('');
});
it('requires confirmation before disconnect and supports cancelling', async () => {
  vi.spyOn(api.myOdoo, 'status').mockResolvedValueOnce(connected).mockResolvedValue(status);
  mount();
  fireEvent.click(await screen.findByRole('button', { name: '斷開' }));
  expect(mockWsClient.call).not.toHaveBeenCalledWith('me.odoo.disconnect', {});
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '取消' }));
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '斷開' }));
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '斷開' }));
  expect(await screen.findByLabelText('API 金鑰')).toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith('me.odoo.disconnect', {});
});
it('reuses the status RPC for the existing member keys wrapper', async () => {
  expect(await api.memberKeys.odooStatus()).toEqual(status);
  expect(mockWsClient.call).toHaveBeenCalledWith('me.odoo.status', {});
});
