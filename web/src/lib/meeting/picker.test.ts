import { afterEach, describe, expect, it, vi } from 'vitest';
import { pickGoogleRecording } from './picker';
import { DRIVE_FILE_SCOPE, type MeetingDriveStatus } from './drive';
const status: MeetingDriveStatus = { connected: true, authorized: true, picker_configured: true, client_id: 'public-client-id', developer_key: 'public-key', app_id: '123', account_email: 'owner@example.com' };
afterEach(() => vi.unstubAllGlobals());
function fixture() {
  let tokenCallback!: (result: { access_token: string; scope: string }) => void;
  let pickerCallback!: (result: { action: string; docs?: { id: string; name: string }[] }) => void;
  const dispose = vi.fn(); const visible = vi.fn(); const token = vi.fn(); const ids = vi.fn();
  const init = vi.fn((config: { callback: typeof tokenCallback }) => { tokenCallback = config.callback; return { requestAccessToken: vi.fn() }; });
  class View { setMimeTypes() { return this; } setFileIds(value: string) { ids(value); return this; } }
  class Builder {
    addView() { return this; } setOAuthToken(value: string) { token(value); return this; }
    setDeveloperKey() { return this; } setAppId() { return this; } setOrigin() { return this; }
    setLocale() { return this; } setCallback(callback: typeof pickerCallback) { pickerCallback = callback; return this; }
    build() { return { setVisible: visible, dispose }; }
  }
  vi.stubGlobal('google', { accounts: { oauth2: { initTokenClient: init } }, picker: { DocsView: View, PickerBuilder: Builder, Action: { PICKED: 'picked', CANCEL: 'cancel' } } });
  return { init, token, ids, dispose, visible, authorize: (scope = DRIVE_FILE_SCOPE) => { const response = { access_token: 'ephemeral-picker-token', scope }; tokenCallback(response); return response; }, pick: () => pickerCallback({ action: 'picked', docs: [{ id: 'abcdefghij123', name: 'Meet.mp4' }] }) };
}
describe('least-scope Google Picker', () => {
  it('uses a separate drive.file-only token and returns only the explicit file selection', async () => {
    const f = fixture(); const abort = new AbortController();
    const promise = pickGoogleRecording(status, 'zh-TW', abort.signal, 'abcdefghij123');
    expect(f.init).toHaveBeenCalledWith(expect.objectContaining({ client_id: 'public-client-id', scope: DRIVE_FILE_SCOPE, include_granted_scopes: false, hint: 'owner@example.com' }));
    const response = f.authorize();
    expect(f.ids).toHaveBeenCalledWith('abcdefghij123'); expect(f.token).toHaveBeenCalledWith('ephemeral-picker-token');
    expect(response.access_token).toBe('');
    f.pick();
    await expect(promise).resolves.toEqual({ id: 'abcdefghij123', name: 'Meet.mp4' });
    expect(f.dispose).toHaveBeenCalledOnce();
  });
  it.each(['https://www.googleapis.com/auth/gmail.readonly', 'https://www.googleapis.com/auth/drive.readonly', 'https://www.googleapis.com/auth/drive'])('refuses broader returned scope %s before handing any token to Picker', async broadScope => {
    const f = fixture();
    const promise = pickGoogleRecording(status, 'en', new AbortController().signal);
    const rejected = expect(promise).rejects.toThrow('meeting.drive.pickerFailed');
    const response = f.authorize(`${DRIVE_FILE_SCOPE} ${broadScope}`);
    await rejected;
    expect(response.access_token).toBe('');
    expect(f.token).not.toHaveBeenCalled(); expect(f.visible).not.toHaveBeenCalled();
  });
  it('accepts explicitly allowlisted identity scopes alongside drive.file', async () => {
    const f = fixture();
    const promise = pickGoogleRecording(status, 'en', new AbortController().signal);
    f.authorize(`${DRIVE_FILE_SCOPE} openid email https://www.googleapis.com/auth/userinfo.email`);
    f.pick();
    await expect(promise).resolves.toEqual({ id: 'abcdefghij123', name: 'Meet.mp4' });
  });
  it('ignores a late OAuth response after the owning dashboard flow is aborted', async () => {
    const f = fixture(); const abort = new AbortController();
    const promise = pickGoogleRecording(status, 'en', abort.signal);
    const rejected = expect(promise).rejects.toMatchObject({ name: 'AbortError' });
    abort.abort(); f.authorize();
    await rejected; expect(f.visible).not.toHaveBeenCalled(); expect(f.token).not.toHaveBeenCalled();
  });
});
