import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Mail } from 'lucide-react';
import { api, type GoogleServiceProbe } from '@/lib/api';
import { formatError } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, Checkbox } from '@/components/mds';
import { IntegrationConnectPanel, OAUTH_REDIRECT_URI } from '@/components/IntegrationConnectPanel';
import { GoogleServiceResults } from './GoogleServiceResults';
import { GoogleCredentialPaths } from '@/components/GoogleCredentialPaths';

export const GOOGLE_SETUP_STORAGE = 'duduclaw.google-setup.v1';
const PATHS = ['delegation', 'internal', 'bridge', 'external'] as const;
type Path = typeof PATHS[number];
type Progress = { step: number; path: Path | null; testing: boolean };
const EMPTY: Progress = { step: 0, path: null, testing: true };

function resume(): Progress {
  try {
    const value = JSON.parse(sessionStorage.getItem(GOOGLE_SETUP_STORAGE) || 'null');
    if (!value || !PATHS.includes(value.path) || !Number.isInteger(value.step) || value.step < 0 || value.step > 5) return EMPTY;
    // A resumed completion must run verification again; no cached success claim.
    return { path: value.path, step: Math.min(value.step, 4), testing: value.testing === true };
  } catch { return EMPTY; }
}

/** Only non-secret navigation progress is persisted while visiting Google Console. */
export function GoogleSetupWizard({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `google.wizard.${key}` });
  const admin = useAuthStore(s => s.user?.role === 'admin');
  const guideLocale = intl.locale.startsWith('zh') ? 'zh-TW/' : intl.locale.startsWith('ja') ? 'ja-JP/' : '';
  const [progress, setProgress] = useState<Progress>(resume);
  const [probes, setProbes] = useState<GoogleServiceProbe[]>([]);
  const [directVerified, setDirectVerified] = useState(false);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [redirectUri, setRedirectUri] = useState(OAUTH_REDIRECT_URI);
  const [requiredScopes, setRequiredScopes] = useState<string[]>([]);
  useEffect(() => {
    if (!admin || !open) return;
    void api.googleCredentials.get().then(status => setRequiredScopes(status.required_scopes)).catch(() => setError(intl.formatMessage({ id: 'google.error.load' })));
    void api.mcp.oauthProviders().then(result => {
      const uri = result.providers.find(provider => provider.provider_id === 'google')?.redirect_uri;
      if (uri) setRedirectUri(uri);
    }).catch(() => setError(intl.formatMessage({ id: 'google.error.load' })));
  }, [admin, open, intl]);
  const oauth = progress.path === 'internal' || progress.path === 'external';
  const go = (step: number) => { setError(''); if (step < 4) { setProbes([]); setBridgeVerified(false); setDirectVerified(false); } setProgress(p => ({ ...p, step })); };
  useEffect(() => {
    if (!admin) return;
    try { sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify(progress)); } catch { /* Storage may be unavailable; the active flow still works. */ }
  }, [progress, admin]);

  const alreadyConnected = async () => {
    setBusy(true); setError('');
    try {
      const status = await api.googleCredentials.get();
      if (oauth) {
        const replacing = ['service_account', 'apps_script'].includes(status.effective);
        if (replacing && !confirm(intl.formatMessage({ id: 'google.cred.replaceConfirm' }))) return;
        await api.googleCredentials.set({ mode: 'oauth', testing: progress.testing, ...(replacing ? { replace_confirmed: true } : {}) });
      } else if (status.effective !== (progress.path === 'bridge' ? 'apps_script' : 'service_account')) {
        setError(t('saveFirst')); return;
      }
      go(3);
    } catch (e) { setError(formatError(e)); }
    finally { setBusy(false); }
  };
  const verify = async () => {
    setBusy(true); setError(''); setProbes([]); setDirectVerified(false);
    try {
      const result = await api.googleCredentials.test();
      setProbes(result.services || []);
      const complete = result.ok && result.path === 'direct' && result.services?.length === 5 && ['gmail', 'calendar', 'drive', 'sheets', 'docs'].every(service => result.services?.some(probe => probe.service === service && probe.ok));
      setDirectVerified(!!complete);
      if (!complete) setError(t('verifyFailed'));
    } catch (e) { setError(formatError(e)); }
    finally { setBusy(false); }
  };
  const [bridgeVerified, setBridgeVerified] = useState(false);
  // Bridge has its own health check, rather than unsupported REST probes.
  const verifyBridge = async () => {
    setBusy(true); setError(''); setBridgeVerified(false);
    try { const result = await api.googleCredentials.test(); setBridgeVerified(result.ok && result.path === 'apps_script'); if (!result.ok || result.path !== 'apps_script') setError(t('verifyFailed')); }
    catch (e) { setError(formatError(e)); }
    finally { setBusy(false); }
  };
  if (!admin) return null;
  const verified = progress.path === 'bridge' ? bridgeVerified : directVerified;
  const close = () => {
    if (progress.step === 5) { setProgress(EMPTY); setProbes([]); setBridgeVerified(false); setDirectVerified(false); }
    onOpenChange(false);
  };
  return <Dialog open={open} onOpenChange={next => next ? onOpenChange(true) : close()}>
    <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
      <DialogHeader><DialogTitle>{t('title')}</DialogTitle></DialogHeader>
      <ol className="flex flex-wrap gap-2 text-xs text-muted-foreground" aria-label={t('steps')}>
        {['situation', 'instructions', 'credentials', 'connect', 'verify', 'done'].map((key, index) => <li key={key} aria-current={progress.step === index ? 'step' : undefined}>{index + 1}. {t(key)}</li>)}
      </ol>
      <a className="text-sm text-brand underline" href={`https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides/${guideLocale}google-setup.md`} target="_blank" rel="noreferrer">{t('guide')}</a>
      {oauth && <div className="space-y-2 rounded-lg border border-surface-border p-3 text-xs"><p>{t('redirect')}</p><code className="block select-all break-all">{redirectUri}</code><Button size="sm" variant="outline" onClick={() => { void navigator.clipboard.writeText(redirectUri).catch(() => setError(t('copyFailed'))); }}>{intl.formatMessage({ id: 'common.copy' })}</Button></div>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {progress.step === 0 && <div className="grid gap-2">{PATHS.map(path => <Button key={path} variant={progress.path === path ? 'default' : 'outline'} onClick={() => { setProbes([]); setBridgeVerified(false); setDirectVerified(false); setProgress({ path, step: 1, testing: path === 'external' }); }}>{t(`path.${path}`)}</Button>)}</div>}
      {progress.step === 1 && progress.path && <div className="space-y-3 text-sm">
        <ol className="list-decimal space-y-2 pl-5">{[1, 2, 3, 4].map(n => <li key={n}>{t(`${progress.path}.${n}`)}</li>)}</ol>
        <div className="flex flex-wrap gap-3">
          {(progress.path === 'bridge' ? [['https://script.google.com/home', 'script']] : progress.path === 'delegation' ? [['https://console.cloud.google.com/iam-admin/serviceaccounts', 'serviceAccounts'], ['https://admin.google.com/ac/owl/domainwidedelegation', 'domainDelegation'], ['https://console.cloud.google.com/apis/library', 'library']] : [
            ['https://console.cloud.google.com/apis/library', 'library'],
            ['https://console.cloud.google.com/auth/branding', 'branding'],
            ['https://console.cloud.google.com/auth/audience', 'audience'],
            ['https://console.cloud.google.com/auth/scopes', 'scopes'],
            ['https://console.cloud.google.com/auth/clients', 'clients'],
          ]).map(([url, label]) => <a key={url} href={url} target="_blank" rel="noreferrer" className="text-brand underline">{t(label)}</a>)}
        </div>
        {progress.path === 'external' && <label className="flex items-start gap-2"><Checkbox checked={progress.testing} onCheckedChange={value => setProgress(p => ({ ...p, testing: value === true }))} />{t('testing')}</label>}
        {oauth && <div className="space-y-2 rounded-lg border border-surface-border p-3"><p>{t('scopes')}</p><p className="break-all font-mono text-xs">{requiredScopes.join(',')}</p><Button size="sm" variant="outline" onClick={() => { void navigator.clipboard.writeText(requiredScopes.join(',')).catch(() => setError(t('copyFailed'))); }}>{intl.formatMessage({ id: 'common.copy' })}</Button></div>}
        <p className="text-muted-foreground">{t('secrets')}</p>
      </div>}
      {progress.step === 2 && (oauth ? <IntegrationConnectPanel providerId="google" prefix="google" headerIcon={Mail} consoleUrl="https://console.cloud.google.com/auth/clients" consoleLabel="Google Cloud Console" clientIdPlaceholder="xxxxxxxx.apps.googleusercontent.com" clientSecretPlaceholder="GOCSPX-..." capabilities={[]} testing={progress.testing} hideRedirect onConnected={() => go(3)} /> : <GoogleCredentialPaths initialMode={progress.path === 'bridge' ? 'apps_script' : 'service_account'} onSaved={() => go(3)} />)}
      {progress.step === 3 && <p className="text-sm">{t('connectedHint')}</p>}
      {progress.step === 4 && <div className="space-y-3">
        <Button disabled={busy} onClick={() => void (progress.path === 'bridge' ? verifyBridge() : verify())}>{busy ? t('checking') : t('runVerification')}</Button>
        {bridgeVerified && <p className="text-sm">{t('bridgeVerified')}</p>}
        <GoogleServiceResults probes={probes} />
      </div>}
      {progress.step === 5 && <p className="text-sm">{t('finished')}</p>}
      <div className="flex flex-wrap justify-between gap-2">
        <Button variant="outline" disabled={busy || progress.step === 0} onClick={() => go(progress.step - 1)}>{t('back')}</Button>
        {progress.step === 1 && <Button onClick={() => go(2)}>{t('next')}</Button>}
        {progress.step === 2 && <Button variant="outline" disabled={busy} onClick={() => void alreadyConnected()}>{t('alreadyConnected')}</Button>}
        {progress.step === 3 && <Button onClick={() => go(4)}>{t('next')}</Button>}
        {progress.step === 4 && <Button disabled={!verified || busy} onClick={() => go(5)}>{t('done')}</Button>}
        {progress.step === 5 && <Button onClick={close}>{t('close')}</Button>}
      </div>
    </DialogContent>
  </Dialog>;
}
