import { fireEvent, screen, within } from '@testing-library/react';
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { IntlProvider } from 'react-intl';
import { mockWsClient } from '@/test/mocks';
import { render } from '@/test/render';
import messages from '@/i18n/zh-TW.json';
import { type PersonalGoogleStatus } from '@/lib/api';
import { DriveWriteMemberCard } from './DriveWriteMemberCard';

const status: PersonalGoogleStatus = {
  drive_write: { connected: false, status: null, account_email: null, granted_scopes: [] }, enabled_by_admin: true,
  connection: { owner_id: 'me', account_email: 'me@example.com', granted_scopes: [], status: 'connected', connected_at: '' },
  login_email: 'me@example.com', client_configured: true, integration_enabled: true, requires_login: false,
  requests: [{ agent_id: 'helper', allowed: true, requesters: 'owner_only', bound_people: [], channels: [], public_visitors_allowed: false }], delegated: [], recent_usage: [],
};
const linked = { ...status, drive_write: { ...status.drive_write, connected: true, account_email: 'ME@example.com', status: 'connected' } };
const started = { authorization_url: 'https://accounts.google.com/fixture', completion: 'redirect', redirect_uri: 'https://dashboard.example/callback', state: 'nonce', expires_in: 300 };
const onRefresh = vi.fn().mockResolvedValue(undefined);
const onStarted = vi.fn();
function mount(value = status) { return render(<IntlProvider locale="zh-TW" messages={messages}><DriveWriteMemberCard status={value} onRefresh={onRefresh} onStarted={onStarted} /></IntlProvider>); }
function checkBoth() { screen.getAllByRole('checkbox').forEach(box => fireEvent.click(box)); }
beforeEach(() => { vi.clearAllMocks(); mockWsClient.call.mockResolvedValue(started); });
afterEach(() => vi.restoreAllMocks());

describe('Drive write member connection', () => {
  it.each([
    [false, 1, false, false], [false, 0, true, false], [true, 0, false, false], [true, 1, false, true], [true, 0, true, true],
  ])('visibility admin=%s requests=%s connected=%s', (enabled, count, connected, visible) => {
    mount({ ...status, enabled_by_admin: enabled, requests: status.requests.slice(0, count), drive_write: { ...status.drive_write, connected } });
    expect(!!screen.queryByText('雲端硬碟寫入')).toBe(visible);
  });
  it('requires both acknowledgements and sends the exact RPC payload in a detached popup', async () => {
    const popup = { opener: window, location: { replace: vi.fn() }, close: vi.fn() };
    vi.spyOn(window, 'open').mockReturnValue(popup as unknown as Window);
    mount();
    const button = screen.getByRole('button', { name: '連結雲端硬碟寫入' });
    expect(button).toBeDisabled();
    const boxes = screen.getAllByRole('checkbox');
    fireEvent.click(boxes[0]); expect(button).toBeDisabled();
    fireEvent.click(boxes[1]); fireEvent.click(boxes[0]); expect(button).toBeDisabled();
    fireEvent.click(boxes[0]); expect(button).toBeEnabled();
    fireEvent.click(button);
    await screen.findByRole('button', { name: '連結雲端硬碟寫入' });
    expect(mockWsClient.call).toHaveBeenCalledWith('me.google.write_connect_start', { redirect_origin: window.location.origin, acknowledge_google_data: true, acknowledge_write: true });
    expect(popup.opener).toBeNull();
    expect(popup.location.replace).toHaveBeenCalledWith(started.authorization_url);
    expect(onStarted).toHaveBeenCalledWith(started);
  });
  it.each(['http://accounts.google.com/path', 'https://accounts.google.com.evil.test', 'https://evil.test', 'javascript:alert(1)'])('rejects unsafe authorization URL %s', async authorization_url => {
    mockWsClient.call.mockResolvedValue({ ...started, authorization_url });
    const popup = { opener: window, location: { replace: vi.fn() }, close: vi.fn() };
    vi.spyOn(window, 'open').mockReturnValue(popup as unknown as Window);
    mount(); checkBoth(); fireEvent.click(screen.getByRole('button', { name: '連結雲端硬碟寫入' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('無法開啟 Google 授權頁面');
    expect(popup.location.replace).not.toHaveBeenCalled(); expect(popup.close).toHaveBeenCalled();
  });
  it('links case-insensitive matching accounts and confirms disconnect before calling the server', async () => {
    mockWsClient.call.mockResolvedValue({ disconnected: true, provider_note: 'Google 提示' });
    mount(linked);
    expect(screen.getByText('已連結')).toBeInTheDocument();
    expect(screen.getByText(/ME@example.com/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '移除寫入連線' }));
    expect(mockWsClient.call).not.toHaveBeenCalled();
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '移除寫入連線' }));
    expect(await screen.findByText('Google 提示')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('me.google.write_disconnect', { confirm: true });
    expect(onRefresh).toHaveBeenCalledOnce();
    expect(screen.getByRole('link', { name: '開啟 Google 第三方存取頁面' })).toHaveAttribute('href', 'https://myaccount.google.com/connections');
  });
  it('requires linking again when accounts differ', () => {
    mount({ ...linked, drive_write: { ...linked.drive_write, account_email: 'other@example.com' } });
    expect(screen.getByText(/寫入連線用的是 other@example.com/)).toHaveTextContent('me@example.com');
    expect(screen.getByRole('button', { name: '重新連結' })).toBeDisabled();
    checkBoth(); expect(screen.getByRole('button', { name: '重新連結' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: '移除寫入連線' })).not.toBeInTheDocument();
  });
  it('shows unreadable status without allowing connection changes', () => {
    mount({ ...status, drive_write: { ...status.drive_write, status: 'unreadable' } });
    expect(screen.getByRole('status')).toHaveTextContent('目前無法讀取雲端硬碟寫入連線狀態，請稍後再試。');
    expect(screen.queryByRole('button')).toBeNull();
  });
  it('preserves a plain refusal', async () => {
    mockWsClient.call.mockRejectedValue(new Error('管理員還沒開放 AI 在雲端硬碟寫入。'));
    vi.spyOn(window, 'open').mockReturnValue(null);
    mount(); checkBoth(); fireEvent.click(screen.getByRole('button', { name: '連結雲端硬碟寫入' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('管理員還沒開放 AI 在雲端硬碟寫入。');
  });
});
