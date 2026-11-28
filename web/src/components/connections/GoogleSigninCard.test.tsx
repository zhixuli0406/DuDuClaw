import { StrictMode } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { render } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import messages from '@/i18n/zh-TW.json';
import { GoogleSigninCard } from './GoogleSigninCard';

const status = { available: true, linked: false, has_password: true };
const linked = { ...status, linked: true, email_at_link: 'member@company.test' };
const authorizationUrl = 'https://accounts.google.com/o/oauth2/v2/auth?state=example';
function mount(strict = false) {
  const card = <IntlProvider locale="zh-TW" messages={messages}><GoogleSigninCard /></IntlProvider>;
  return render(strict ? <StrictMode>{card}</StrictMode> : card);
}
async function openLink() {
  fireEvent.click(await screen.findByRole('button', { name: '開啟 Google 登入' }));
  expect(screen.getByRole('dialog', { name: '確認身分' })).toBeInTheDocument();
  expect(screen.getByText('輸入目前的密碼，接著會前往 Google 登入。')).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText('目前密碼'), { target: { value: 'current-password' } });
  fireEvent.click(screen.getByRole('button', { name: '前往 Google' }));
}
beforeEach(() => {
  vi.restoreAllMocks(); mockWsClient.call.mockReset();
  window.history.replaceState(null, '', '/me/connections');
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.google_signin.status') return status;
    if (method === 'me.google_signin.link_start') return { authorization_url: authorizationUrl };
    if (method === 'me.google_signin.unlink') return { unlinked: true };
    return {};
  });
});
afterEach(() => { vi.unstubAllGlobals(); window.history.replaceState(null, '', '/'); });

it('shows the not-linked state and verifies the password before navigating the whole page to Google', async () => {
  const assign = vi.fn();
  vi.stubGlobal('location', { origin: window.location.origin, hash: '', assign });
  mount();
  expect(await screen.findByText('開啟後，你可以用公司 Google 帳號登入儀表板，不必輸入密碼。')).toBeInTheDocument();
  expect(screen.getByText('開啟前要先輸入密碼確認身分。')).toBeInTheDocument();
  await openLink();
  await waitFor(() => expect(assign).toHaveBeenCalledWith(authorizationUrl));
  expect(mockWsClient.call).toHaveBeenCalledWith('me.google_signin.link_start', { password: 'current-password' });
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '開啟 Google 登入' }));
  expect(screen.getByLabelText('目前密碼')).toHaveValue('');
});
it.each([
  'https://accounts.google.com.evil.test/auth', 'http://accounts.google.com/auth',
  'https://accounts.google.com:444/auth', 'javascript:alert(1)', '/relative',
  'https://user:secret@accounts.google.com/auth', 'invalid url',
])('refuses an unsafe authorization URL and clears the password: %s', async authorization_url => {
  const assign = vi.fn();
  vi.stubGlobal('location', { origin: window.location.origin, hash: '', assign });
  mockWsClient.call.mockImplementation(async (method: string) => method === 'me.google_signin.status' ? status : { authorization_url });
  mount(); await openLink();
  expect(await screen.findByRole('alert')).toHaveTextContent('無法開啟 Google 登入頁面，請稍後再試。');
  expect(assign).not.toHaveBeenCalled();
  expect(screen.getByLabelText('目前密碼')).toHaveValue('');
});
it('shows the linked email and requires a password to unlink, then refreshes status', async () => {
  let isLinked = true;
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.google_signin.status') return isLinked ? linked : status;
    if (method === 'me.google_signin.unlink') { isLinked = false; return { unlinked: true }; }
    return {};
  });
  mount();
  expect(await screen.findByText('已開啟')).toBeInTheDocument();
  expect(screen.getByText(linked.email_at_link)).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '取消 Google 登入' }));
  const dialog = within(screen.getByRole('dialog'));
  expect(dialog.getByText('取消也要輸入密碼。')).toBeInTheDocument();
  expect(dialog.getByRole('button', { name: '取消 Google 登入' })).toBeDisabled();
  fireEvent.change(dialog.getByLabelText('目前密碼'), { target: { value: 'unlink-password' } });
  fireEvent.click(dialog.getByRole('button', { name: '取消 Google 登入' }));
  expect(await screen.findByRole('button', { name: '開啟 Google 登入' })).toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith('me.google_signin.unlink', { password: 'unlink-password' });
  fireEvent.click(screen.getByRole('button', { name: '開啟 Google 登入' }));
  expect(screen.getByLabelText('目前密碼')).toHaveValue('');
});
it('disables unlink without a local password even when company sign-in is unavailable', async () => {
  mockWsClient.call.mockResolvedValue({ ...linked, available: false, has_password: false });
  mount();
  expect(await screen.findByRole('button', { name: '取消 Google 登入' })).toBeDisabled();
  expect(screen.getByText('你的帳號是用 Google 開通的，沒有設定密碼，不能取消 Google 登入。')).toBeInTheDocument();
});
it('hides the block when unavailable and not linked', async () => {
  mockWsClient.call.mockResolvedValue({ ...status, available: false });
  const { container } = mount();
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.google_signin.status', {}));
  expect(container).toBeEmptyDOMElement();
});
it('immediately removes the ticket while preserving history and query, completes once under StrictMode, and refreshes status', async () => {
  window.history.replaceState({ retained: true }, '', '/me/connections?keep=yes#gsi_link=one-time_ticket');
  const replace = vi.spyOn(window.history, 'replaceState');
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.google_signin.link_complete') {
      expect(window.location.hash).toBe('');
      return {};
    }
    return linked;
  });
  mount(true);
  expect(window.location.hash).toBe('');
  expect(replace).toHaveBeenCalledWith({ retained: true }, '', '/me/connections?keep=yes');
  expect(await screen.findByText('已開啟')).toBeInTheDocument();
  const calls = mockWsClient.call.mock.calls;
  expect(calls.filter(([method]) => method === 'me.google_signin.link_complete')).toEqual([['me.google_signin.link_complete', { ticket: 'one-time_ticket' }]]);
  expect(calls.map(([method]) => method)).toEqual(['me.google_signin.link_complete', 'me.google_signin.status']);
});
it.each(['#gsi_link=', '#gsi_link=bad%20ticket', '#gsi_link=valid&extra=value', '#other=value'])('ignores a non-matching callback %s', async hash => {
  window.history.replaceState(null, '', `/me/connections${hash}`);
  mount();
  await screen.findByRole('button', { name: '開啟 Google 登入' });
  expect(mockWsClient.call).not.toHaveBeenCalledWith('me.google_signin.link_complete', expect.anything());
  expect(window.location.hash).toBe(hash);
});
it.each([
  ['signin_sub_linked', '這個 Google 帳號已經被其他成員使用'],
  ['signin_personal_sub_mismatch', '請使用與個人 Google 連線相同的 Google 帳號。'],
])('displays the server refusal for %s and still refreshes status', async (code, message) => {
  window.history.replaceState(null, '', '/me/connections#gsi_link=refused-ticket');
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.google_signin.link_complete') throw { code, message };
    return status;
  });
  mount(true);
  expect(await screen.findByRole('alert')).toHaveTextContent(message);
  expect(await screen.findByRole('button', { name: '開啟 Google 登入' })).toBeInTheDocument();
  expect(window.location.hash).toBe('');
  expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'me.google_signin.link_complete')).toHaveLength(1);
});
it('clears the password immediately during an attempt and after refusal', async () => {
  let refuse!: (reason: unknown) => void;
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.google_signin.link_start') return new Promise((_, reject) => { refuse = reject; });
    return status;
  });
  mount(); await openLink();
  expect(screen.getByLabelText('目前密碼')).toHaveValue('');
  expect(screen.getByRole('button', { name: '前往 Google' })).toBeDisabled();
  refuse({ code: 'signin_password_invalid', message: '密碼不正確，請重新輸入。' });
  expect(await screen.findByRole('alert')).toHaveTextContent('密碼不正確，請重新輸入。');
  expect(screen.getByLabelText('目前密碼')).toHaveValue('');
});
it('clears the password when cancelling a dialog', async () => {
  mount();
  fireEvent.click(await screen.findByRole('button', { name: '開啟 Google 登入' }));
  fireEvent.change(screen.getByLabelText('目前密碼'), { target: { value: 'cancelled-password' } });
  fireEvent.click(screen.getByRole('button', { name: '取消' }));
  fireEvent.click(screen.getByRole('button', { name: '開啟 Google 登入' }));
  expect(screen.getByLabelText('目前密碼')).toHaveValue('');
});
