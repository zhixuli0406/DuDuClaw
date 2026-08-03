import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { CheckCircle2, AlertTriangle, KeyRound, Copy, Check, Loader2 } from 'lucide-react';
import { api, type GoogleCredentialsStatus } from '@/lib/api';
import { toast } from '@/lib/toast';
import {
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  Button,
  Badge,
  Input,
  Segmented,
  Separator,
} from '@/components/mds';

type Mode = 'oauth' | 'service_account' | 'apps_script';

/**
 * GoogleCredentialPaths — configure the two Google credential paths that need
 * no per-customer OAuth client (see `docs/guides/google-no-oauth-client.md`).
 *
 * Sits under the OAuth connect panel on the Google integration tab. The three
 * paths authorize the SAME nineteen tools, so this is a "pick one" surface
 * rather than three independent switches: saving a mode clears the other's
 * config server-side, which keeps the configured path and the effective path
 * from disagreeing.
 *
 * The stored bridge secret is never sent back to the browser. Leaving the
 * secret field blank when editing a saved bridge keeps the existing one.
 */
export function GoogleCredentialPaths() {
  const intl = useIntl();
  const t = (id: string, defaultMessage: string, values?: Record<string, string>) =>
    intl.formatMessage({ id, defaultMessage }, values);

  const [status, setStatus] = useState<GoogleCredentialsStatus | null>(null);
  const [mode, setMode] = useState<Mode>('oauth');
  const [keyFile, setKeyFile] = useState('');
  const [subject, setSubject] = useState('');
  const [url, setUrl] = useState('');
  const [secret, setSecret] = useState('');
  const [busy, setBusy] = useState<'idle' | 'saving' | 'testing'>('idle');
  const [copied, setCopied] = useState(false);

  const load = useCallback(async () => {
    try {
      const s = await api.googleCredentials.get();
      setStatus(s);
      setKeyFile(s.service_account.key_file ?? '');
      setSubject(s.service_account.subject ?? '');
      setUrl(s.apps_script.url ?? '');
      // Reflect what is actually configured so the form opens on the path the
      // operator is already using.
      if (s.service_account.configured) setMode('service_account');
      else if (s.apps_script.configured) setMode('apps_script');
      else setMode('oauth');
    } catch {
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    setBusy('saving');
    try {
      if (mode === 'oauth') {
        await api.googleCredentials.set({ mode: 'none' });
      } else if (mode === 'service_account') {
        await api.googleCredentials.set({ mode: 'service_account', key_file: keyFile, subject });
      } else {
        await api.googleCredentials.set({
          mode: 'apps_script',
          url,
          // Blank means "keep the stored secret" — the server preserves it.
          ...(secret.trim() ? { secret: secret.trim() } : {}),
        });
      }
      setSecret('');
      toast.success(t('google.cred.saved', '已儲存'));
      await load();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy('idle');
    }
  };

  const test = async () => {
    setBusy('testing');
    try {
      const r = await api.googleCredentials.test();
      toast.success(r.account ? `${r.detail}（${r.account}）` : r.detail);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy('idle');
    }
  };

  const copyScopes = async () => {
    if (!status) return;
    await navigator.clipboard.writeText(status.required_scopes.join(','));
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  if (!status) return null;

  const effectiveLabel =
    status.effective === 'direct'
      ? t('google.cred.effective.direct', '直接 API 權杖（OAuth 或服務帳號）· 19 個工具全通')
      : status.effective === 'apps_script'
        ? t('google.cred.effective.bridge', 'Apps Script 橋接 · Gmail／行事曆／Sheets')
        : t('google.cred.effective.none', '尚未設定任何憑證');

  const brokenSection =
    status.service_account.error || status.apps_script.error || '';

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <KeyRound className="size-4" />
          {t('google.cred.title', '憑證方式')}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <p className="text-sm text-muted-foreground">
          {t(
            'google.cred.intro',
            '三種授權方式對應同一組 19 個工具。選一種即可 — 儲存時會清掉另一種的設定，避免實際生效的跟你以為的不一致。',
          )}
        </p>

        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted-foreground">{t('google.cred.effective', '目前生效')}</span>
          <Badge variant={status.effective === 'none' ? 'outline' : 'default'}>
            {effectiveLabel}
          </Badge>
        </div>

        {/* The master gate is separate from the credential itself. Without this
            warning an operator can configure a path, watch 測試連線 go green, and
            still find no Google tools reach their agents — the test exercises the
            credential, not the gate. */}
        {!status.integration_enabled && (
          <div className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
            <span>
              {t(
                'google.cred.gateOff',
                'Google 整合的總開關還沒開，工具不會出現在 AI 員工面前（憑證測試仍會通過）。要開啟請在 config.toml 加上 [integrations] google_workspace = true。',
              )}
            </span>
          </div>
        )}

        {/* A section that exists but cannot be used must be visible here — it
            would otherwise read as "not configured" while tools fail. */}
        {brokenSection && (
          <div className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
            <span>{brokenSection}</span>
          </div>
        )}

        <Separator />

        <Segmented<Mode>
          value={mode}
          onValueChange={setMode}
          aria-label={t('google.cred.title', '憑證方式')}
          options={[
            { value: 'oauth', label: t('google.cred.mode.oauth', 'OAuth 連線') },
            { value: 'service_account', label: t('google.cred.mode.sa', '服務帳號委派') },
            { value: 'apps_script', label: t('google.cred.mode.gas', 'Apps Script 橋接') },
          ]}
        />

        {mode === 'oauth' && (
          <p className="text-sm text-muted-foreground">
            {t(
              'google.cred.oauth.hint',
              '用上方的連線面板授權。需要自備 Google OAuth client；個人帳號與 Workspace 都適用，19 個工具全通。',
            )}
          </p>
        )}

        {mode === 'service_account' && (
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">
              {t(
                'google.cred.sa.hint',
                '客戶的 Workspace 超級管理員在 Admin console 授權一次即可，不需要 Google 應用程式驗證。僅支援 Workspace 網域，個人 @gmail.com 無法委派。',
              )}
            </p>
            <div className="space-y-1.5">
              <label className="text-xs font-medium" htmlFor="gcred-keyfile">
                {t('google.cred.sa.keyFile', '服務帳號金鑰檔路徑')}
              </label>
              <Input
                id="gcred-keyfile"
                value={keyFile}
                onChange={(e) => setKeyFile(e.target.value)}
                placeholder="keys/google-sa.json"
              />
              <p className="text-xs text-muted-foreground">
                {t('google.cred.sa.keyFileHint', '相對路徑以 ~/.duduclaw 為基準。建議 chmod 600。')}
              </p>
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-medium" htmlFor="gcred-subject">
                {t('google.cred.sa.subject', '要代理的使用者信箱')}
              </label>
              <Input
                id="gcred-subject"
                value={subject}
                onChange={(e) => setSubject(e.target.value)}
                placeholder="boss@customer.com"
              />
            </div>
            <div className="rounded-lg border border-border bg-muted/40 p-3">
              <div className="flex items-center justify-between gap-2">
                <span className="text-xs font-medium">
                  {t('google.cred.sa.scopes', '交給管理員的 scope 清單')}
                </span>
                <Button variant="outline" size="sm" onClick={copyScopes}>
                  {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
                  {copied ? t('common.copied', '已複製') : t('common.copy', '複製')}
                </Button>
              </div>
              <p className="mt-1.5 break-all font-mono text-[10px] leading-4 text-muted-foreground">
                {status.required_scopes.join(',')}
              </p>
            </div>
          </div>
        )}

        {mode === 'apps_script' && (
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">
              {t(
                'google.cred.gas.hint',
                '使用者在自己的 Google 帳號部署一份腳本，個人 @gmail.com 也能用。只涵蓋 Gmail、行事曆與 Sheets。',
              )}
            </p>
            <div className="space-y-1.5">
              <label className="text-xs font-medium" htmlFor="gcred-url">
                {t('google.cred.gas.url', '網頁應用程式網址')}
              </label>
              <Input
                id="gcred-url"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="https://script.google.com/macros/s/.../exec"
              />
              <p className="text-xs text-muted-foreground">
                {t('google.cred.gas.urlHint', '要用 /exec 結尾的發布網址，不是 /dev。')}
              </p>
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-medium" htmlFor="gcred-secret">
                {t('google.cred.gas.secret', '共用密鑰')}
              </label>
              <Input
                id="gcred-secret"
                type="password"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={
                  status.apps_script.configured
                    ? t('google.cred.gas.secretKeep', '留白 = 沿用已儲存的密鑰')
                    : t('google.cred.gas.secretNew', '腳本裡 SECRET 的值')
                }
              />
              <p className="text-xs text-muted-foreground">
                {t(
                  'google.cred.gas.secretHint',
                  '網址加密鑰等同一組密碼，兩者合起來就能讀信。加密存放，不會再顯示。',
                )}
              </p>
            </div>
          </div>
        )}

        <div className="flex items-center gap-2">
          <Button onClick={save} disabled={busy !== 'idle'}>
            {busy === 'saving' && <Loader2 className="size-4 animate-spin" />}
            {t('common.save', '儲存')}
          </Button>
          <Button variant="outline" onClick={test} disabled={busy !== 'idle'}>
            {busy === 'testing' ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <CheckCircle2 className="size-4" />
            )}
            {t('google.cred.test', '測試連線')}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
