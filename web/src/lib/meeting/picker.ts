import { DRIVE_FILE_SCOPE, type MeetingDriveStatus, type PickedRecording } from './drive';

// Only the public Google SDK surface used here. No token enters app state or storage.
interface PickerView { setMimeTypes(types: string): PickerView; setFileIds(ids: string): PickerView }
interface Picker { setVisible(visible: boolean): void; dispose(): void }
interface PickerResult { action: string; docs?: { id?: string; name?: string }[] }
interface PickerBuilder {
  addView(view: PickerView): PickerBuilder;
  setOAuthToken(token: string): PickerBuilder;
  setDeveloperKey(key: string): PickerBuilder;
  setAppId(id: string): PickerBuilder;
  setOrigin(origin: string): PickerBuilder;
  setLocale(locale: string): PickerBuilder;
  setCallback(callback: (result: PickerResult) => void): PickerBuilder;
  build(): Picker;
}
interface GoogleSdk {
  accounts: { oauth2: { initTokenClient(config: {
    client_id: string; scope: string; include_granted_scopes: false; hint: string;
    callback: (response: { access_token?: string; error?: string; scope?: string }) => void;
    error_callback: () => void;
  }): { requestAccessToken(): void } } };
  picker: { DocsView: new () => PickerView; PickerBuilder: new () => PickerBuilder; Action: { PICKED: string; CANCEL: string } };
}
type SdkWindow = Window & { google?: GoogleSdk; gapi?: { load(name: string, options: { callback: () => void; onerror: () => void; timeout: number; ontimeout: () => void }): void } };
const sdkWindow = () => window as SdkWindow;
const MIME_TYPES = 'audio/mpeg,audio/mp3,audio/mp4,audio/x-m4a,audio/wav,audio/x-wav,audio/flac,audio/ogg,audio/aac,audio/aiff,audio/x-aiff,audio/webm,video/mp4,video/webm,video/quicktime,video/x-matroska';
let ready: Promise<void> | undefined;
function script(src: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const el = document.createElement('script');
    const timer = window.setTimeout(() => { el.remove(); reject(new Error('meeting.drive.pickerFailed')); }, 15_000);
    el.src = src; el.async = true; el.referrerPolicy = 'strict-origin-when-cross-origin';
    el.onload = () => { window.clearTimeout(timer); resolve(); };
    el.onerror = () => { window.clearTimeout(timer); el.remove(); reject(new Error('meeting.drive.pickerFailed')); };
    document.head.appendChild(el);
  });
}
export function prepareGooglePicker(): Promise<void> {
  if (!ready) ready = (async () => {
    await Promise.all([
      sdkWindow().google?.accounts ? Promise.resolve() : script('https://accounts.google.com/gsi/client'),
      sdkWindow().gapi ? Promise.resolve() : script('https://apis.google.com/js/api.js'),
    ]);
    await new Promise<void>((resolve, reject) => {
      const fail = () => reject(new Error('meeting.drive.pickerFailed'));
      if (!sdkWindow().gapi) { fail(); return; }
      sdkWindow().gapi!.load('picker', { callback: resolve, onerror: fail, timeout: 15_000, ontimeout: fail });
    });
  })().catch(error => { ready = undefined; throw error; });
  return ready;
}
export function pickGoogleRecording(config: MeetingDriveStatus, locale: string, signal: AbortSignal, fileId?: string): Promise<PickedRecording | null> {
  return new Promise((resolve, reject) => {
    const google = sdkWindow().google;
    if (signal.aborted) { reject(new DOMException('Aborted', 'AbortError')); return; }
    if (!google?.accounts || !google.picker || !config.picker_configured || !config.client_id || !config.developer_key || !config.app_id || !config.account_email) {
      reject(new Error('meeting.drive.pickerFailed')); return;
    }
    let picker: Picker | undefined;
    let settled = false;
    const finish = (result: PickedRecording | null, error?: Error) => {
      if (settled) return;
      settled = true; signal.removeEventListener('abort', abort); picker?.dispose(); picker = undefined;
      if (error) reject(error); else resolve(result);
    };
    const abort = () => finish(null, new DOMException('Aborted', 'AbortError'));
    signal.addEventListener('abort', abort, { once: true });
    try {
      // Separate, ephemeral drive.file-only token: never expose the stored personal token.
      const client = google.accounts.oauth2.initTokenClient({
        client_id: config.client_id, scope: DRIVE_FILE_SCOPE, include_granted_scopes: false, hint: config.account_email,
        error_callback: () => finish(null, new Error('meeting.drive.pickerFailed')),
        callback: response => {
          if (settled || signal.aborted) { response.access_token = ''; return; }
          const scopes = response.scope?.trim().split(/\s+/) ?? [];
          const allowedScopes = [DRIVE_FILE_SCOPE, 'openid', 'email', 'https://www.googleapis.com/auth/userinfo.email'];
          if (response.error || !response.access_token || !scopes.includes(DRIVE_FILE_SCOPE) || scopes.some(scope => !allowedScopes.includes(scope))) {
            response.access_token = '';
            finish(null, new Error('meeting.drive.pickerFailed')); return;
          }
          try {
            const view = new google.picker.DocsView().setMimeTypes(MIME_TYPES);
            if (fileId) view.setFileIds(fileId);
            picker = new google.picker.PickerBuilder().addView(view).setOAuthToken(response.access_token)
              .setDeveloperKey(config.developer_key!).setAppId(config.app_id!).setOrigin(window.location.origin)
              .setLocale(locale === 'zh-TW' ? 'zh-TW' : locale === 'ja-JP' ? 'ja' : 'en')
              .setCallback(result => {
                if (result.action === google.picker.Action.CANCEL) finish(null);
                if (result.action === google.picker.Action.PICKED) {
                  const selected = result.docs?.[0];
                  if (!selected?.id || !/^[A-Za-z0-9_-]{10,200}$/.test(selected.id)) { finish(null, new Error('meeting.drive.pickerFailed')); return; }
                  finish({ id: selected.id, name: selected.name || selected.id });
                }
              }).build();
            picker.setVisible(true);
          } catch { finish(null, new Error('meeting.drive.pickerFailed')); }
          finally { response.access_token = ''; }
        },
      });
      client.requestAccessToken();
    } catch { finish(null, new Error('meeting.drive.pickerFailed')); }
  });
}
