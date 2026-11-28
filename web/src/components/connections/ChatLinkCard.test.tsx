import { act, fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { IntlProvider } from 'react-intl';
import '@/test/mocks';
import { render } from '@/test/render';
import messages from '@/i18n/zh-TW.json';
import { client } from '@/lib/ws-client';
import { api } from '@/lib/api';
import { ChatLinkCard } from './ChatLinkCard';

const waiting = { handshake_id: 'h1', status: 'waiting' as const, channel: 'telegram' as const, expires_at: '2099-01-01T00:00:00Z', display_name: null, masked_target: null };
const matched = { ...waiting, status: 'matched' as const, display_name: '<img src=x onerror=alert(1)>', masked_target: '編號末四碼 4821' };
const identity = { channel: 'telegram' as const, channel_user_id: '9876543214821', verified: true, proof_status: 'self_confirmed' as const };
function mount() { return render(<IntlProvider locale="zh-TW" messages={messages}><ChatLinkCard /></IntlProvider>); }
async function start() { mount(); fireEvent.click(await screen.findByRole('button', { name: '產生代碼' })); await screen.findByText('DUDU-7K3P-Q9MX'); }
async function startWithTimers() {
  vi.useFakeTimers();
  const view = mount();
  await act(async () => {});
  await act(async () => { fireEvent.click(screen.getByRole('button', { name: '產生代碼' })); });
  return view;
}
beforeEach(() => {
  vi.spyOn(api.chatLink, 'list').mockResolvedValue({ identities: [], handshakes: [] });
  vi.spyOn(api.chatLink, 'start').mockResolvedValue({ ...waiting, code: 'DUDU-7K3P-Q9MX', targets: [{ channel: 'telegram', account: 'secret-account', agent_id: 'secret-agent', display_name: 'Office', bot_username: 'office_bot' }] });
  vi.spyOn(api.chatLink, 'status').mockResolvedValue(waiting);
  vi.spyOn(api.chatLink, 'confirm').mockResolvedValue({ confirmed: true });
  vi.spyOn(api.chatLink, 'reject').mockResolvedValue({ rejected: true });
  vi.spyOn(api.chatLink, 'cancel').mockResolvedValue({ cancelled: true });
  vi.spyOn(api.chatLink, 'remove').mockResolvedValue({ revoked: true });
});
afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });
it('starts with a code and a destination without internal identifiers', async () => {
  await start();
  expect(api.chatLink.start).toHaveBeenCalledWith('telegram');
  expect(screen.getByText(/私訊 @office_bot/)).toBeInTheDocument();
  expect(document.body.textContent).not.toContain('secret-account');
  expect(document.body.textContent).not.toContain('secret-agent');
});
it('polls to matched, renders the display name as text and stops on unmount', async () => {
  const view = await startWithTimers(); vi.mocked(api.chatLink.status).mockResolvedValue(matched);
  await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
  expect(screen.getByText(/<img src=x onerror=alert\(1\)>/)).toBeInTheDocument();
  expect(document.querySelector('img')).toBeNull();
  view.unmount();
  const calls = vi.mocked(api.chatLink.status).mock.calls.length;
  await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
  expect(api.chatLink.status).toHaveBeenCalledTimes(calls);
});
it('confirms the matched account and reloads the proof', async () => {
  vi.mocked(api.chatLink.list).mockResolvedValueOnce({ identities: [], handshakes: [matched] }).mockResolvedValue({ identities: [identity], handshakes: [] });
  mount(); fireEvent.click(await screen.findByRole('button', { name: '是我' }));
  expect(await screen.findByText('本人確認')).toBeInTheDocument();
  expect(api.chatLink.confirm).toHaveBeenCalledWith('h1');
});
it('rejects the match and invalidates the code', async () => {
  vi.mocked(api.chatLink.list).mockResolvedValue({ identities: [], handshakes: [matched] });
  mount(); fireEvent.click(await screen.findByRole('button', { name: '不是我' }));
  expect(await screen.findByText('有人用了你的代碼，請重新產生。這組代碼已作廢。')).toBeInTheDocument();
  expect(api.chatLink.reject).toHaveBeenCalledWith('h1');
});
it('shows expiration and stops polling', async () => {
  await startWithTimers(); vi.mocked(api.chatLink.status).mockResolvedValue({ ...waiting, status: 'expired' });
  await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
  expect(screen.getByText('代碼已失效，請重新產生。')).toBeInTheDocument();
  const calls = vi.mocked(api.chatLink.status).mock.calls.length;
  await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
  expect(api.chatLink.status).toHaveBeenCalledTimes(calls);
});
it('shows the server start refusal verbatim', async () => {
  vi.mocked(api.chatLink.start).mockRejectedValue(new Error('產生次數已達上限，請稍後再試。'));
  mount(); fireEvent.click(await screen.findByRole('button', { name: '產生代碼' }));
  expect(await screen.findByText('產生次數已達上限，請稍後再試。')).toBeInTheDocument();
});
it('masks identities, excludes proven channels and confirms proof removal', async () => {
  vi.mocked(api.chatLink.list).mockResolvedValue({ identities: [identity], handshakes: [] });
  mount(); expect(await screen.findByText(/編號末四碼 4821/)).toBeInTheDocument();
  expect(document.body.textContent).not.toContain(identity.channel_user_id);
  fireEvent.click(screen.getByRole('button', { name: '取消聊天核准資格' }));
  expect(api.chatLink.remove).not.toHaveBeenCalled();
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '取消聊天核准資格' }));
  await act(async () => {});
  expect(api.chatLink.remove).toHaveBeenCalledWith('telegram', identity.channel_user_id);
  fireEvent.click(screen.getByRole('button', { name: '連結另一個通道' }));
  expect(screen.queryByRole('radio', { name: 'Telegram' })).not.toBeInTheDocument();
});
it('resumes waiting without showing a code', async () => {
  vi.mocked(api.chatLink.list).mockResolvedValue({ identities: [], handshakes: [waiting] });
  mount(); expect(await screen.findByText('代碼已產生，請到聊天軟體貼上；如果找不到代碼，請取消後重新產生。')).toBeInTheDocument();
  expect(screen.queryByText(/DUDU-/)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '取消' }));
  await act(async () => {}); expect(api.chatLink.cancel).toHaveBeenCalledWith('h1');
});
it('explains prefilled identities', async () => {
  vi.mocked(api.chatLink.list).mockResolvedValue({ identities: [{ ...identity, proof_status: 'admin_prefilled' }], handshakes: [] });
  mount(); expect(await screen.findByText('尚未本人確認')).toBeInTheDocument();
  expect(screen.getByRole('button', { name: '產生代碼完成本人確認' })).toBeInTheDocument();
});

it('uses the exact RPC methods and fields', async () => {
  vi.restoreAllMocks();
  const call = vi.spyOn(client, 'call').mockResolvedValue({});
  await api.chatLink.start('googlechat');
  await api.chatLink.status('h1');
  await api.chatLink.confirm('h1');
  await api.chatLink.reject('h1');
  await api.chatLink.cancel('h1');
  await api.chatLink.list();
  await api.chatLink.remove('telegram', identity.channel_user_id);
  expect(call.mock.calls).toEqual([
    ['me.chat_link.start', { channel: 'googlechat' }],
    ['me.chat_link.status', { handshake_id: 'h1' }],
    ['me.chat_link.confirm', { handshake_id: 'h1' }],
    ['me.chat_link.reject', { handshake_id: 'h1' }],
    ['me.chat_link.cancel', { handshake_id: 'h1' }],
    ['me.chat_link.list', {}],
    ['me.chat_link.remove', { channel: 'telegram', channel_user_id: identity.channel_user_id }],
  ]);
});
it('uses the Google Chat app name from targets', async () => {
  vi.mocked(api.chatLink.start).mockResolvedValue({ ...waiting, code: 'DUDU-7K3P-Q9MX', targets: [{ channel: 'googlechat', account: 'secret-account', agent_id: 'secret-agent', display_name: '嘟嘟辦公室', project_number: '12345678' }] });
  mount(); fireEvent.click(await screen.findByRole('radio', { name: 'Google Chat' }));
  fireEvent.click(screen.getByRole('button', { name: '產生代碼' }));
  expect(await screen.findByText(/私訊「嘟嘟辦公室」/)).toBeInTheDocument();
  expect(api.chatLink.start).toHaveBeenCalledWith('googlechat');
  expect(document.body.textContent).not.toContain('12345678');
});
it('shows confirm refusals and keeps the match available', async () => {
  vi.mocked(api.chatLink.list).mockResolvedValue({ identities: [], handshakes: [matched] });
  vi.mocked(api.chatLink.confirm).mockRejectedValue(new Error('請在產生代碼的登入工作階段確認。'));
  mount(); fireEvent.click(await screen.findByRole('button', { name: '是我' }));
  expect(await screen.findByRole('alert')).toHaveTextContent('請在產生代碼的登入工作階段確認。');
  expect(screen.getByRole('button', { name: '是我' })).toBeEnabled();
});
it('expires locally when the countdown reaches zero', async () => {
  vi.useFakeTimers();
  vi.mocked(api.chatLink.list).mockResolvedValue({ identities: [], handshakes: [{ ...waiting, expires_at: new Date(Date.now() + 1000).toISOString() }] });
  mount(); await act(async () => {});
  expect(screen.getByRole('timer')).toHaveTextContent('00:01');
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  expect(screen.getByText('代碼已失效，請重新產生。')).toBeInTheDocument();
  expect(api.chatLink.status).not.toHaveBeenCalled();
});
