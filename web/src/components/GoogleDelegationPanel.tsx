import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, Check, Copy, Loader2, Upload, Users } from 'lucide-react';
import { useNavigate } from 'react-router';
import { api, type GoogleDelegationStatus } from '@/lib/api';
import { toast } from '@/lib/toast';
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Input, Separator } from '@/components/mds';

/** Largest key file the browser reads (a real one is ~2.3 KB). */
const MAX_KEY_BYTES = 16 * 1024;

/**
 * 公司授權代表 — one service account with domain-wide delegation, and per AI
 * employee the colleague it acts as. The key is sealed by the gateway with
 * the credential-protection key and never comes back to the browser.
 */
export function GoogleDelegationPanel() {
  const intl = useIntl();
  const navigate = useNavigate();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [status, setStatus] = useState<GoogleDelegationStatus | null>(null);
  const [domains, setDomains] = useState('');
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState<'scopes' | 'client' | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      const s = await api.googleIdentity.delegation();
      setStatus(s);
      setDomains((s.settings?.allowed_subject_domains ?? []).join(', '));
    } catch {
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const run = async (f: () => Promise<unknown>, done: string) => {
    setBusy(true);
    try {
      await f();
      toast.success(t(done));
      await load();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onFile = async (file: File | undefined) => {
    if (!file) return;
    if (file.size > MAX_KEY_BYTES) {
      toast.error(t('googleDelegation.key.tooLarge'));
      return;
    }
    const text = await file.text();
    await run(() => api.googleIdentity.importKey(text), 'googleDelegation.key.imported');
    if (fileRef.current) fileRef.current.value = '';
  };

  const copy = async (what: 'scopes' | 'client', text: string) => {
    await navigator.clipboard.writeText(text);
    setCopied(what);
    setTimeout(() => setCopied(null), 2000);
  };

  if (!status) return null;
  const protectionOk = status.protection.person_identity_allowed;
  const delegates = status.agents.filter((a) => a.kind === 'delegate');

  return (
    <Card data-testid="google-delegation">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Users className="size-4" />
          {t('googleDelegation.title')}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <p className="text-sm text-muted-foreground">{t('googleDelegation.intro')}</p>

        <div className="flex flex-wrap items-center gap-2 text-sm">
          <Badge variant={protectionOk ? 'default' : 'outline'}>{status.protection.summary}</Badge>
        </div>
        {!protectionOk && (
          <div className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
            <span>{t('googleDelegation.protectionNeeded')}</span>
          </div>
        )}

        <Separator />

        <div className="space-y-2">
          <p className="text-sm font-medium">{t('googleDelegation.key.title')}</p>
          {status.key ? (
            <div className="space-y-1 text-sm">
              <p>
                {t('googleDelegation.key.account')} <span className="font-mono text-xs">{status.key.client_email}</span>
              </p>
              {status.key.client_id && (
                <p className="flex flex-wrap items-center gap-2">
                  {t('googleDelegation.key.clientId')}
                  <span className="font-mono text-xs">{status.key.client_id}</span>
                  <Button variant="outline" size="sm" onClick={() => void copy('client', status.key?.client_id ?? '')}>
                    {copied === 'client' ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
                  </Button>
                </p>
              )}
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">{t('googleDelegation.key.none')}</p>
          )}
          {status.key_error && <p className="text-xs text-warning">{status.key_error}</p>}
          <div className="flex flex-wrap gap-2">
            <input
              ref={fileRef}
              type="file"
              accept=".json,application/json"
              className="hidden"
              onChange={(e) => void onFile(e.target.files?.[0])}
              data-testid="google-delegation-file"
            />
            <Button onClick={() => fileRef.current?.click()} disabled={busy || !protectionOk}>
              {busy ? <Loader2 className="size-4 animate-spin" /> : <Upload className="size-4" />}
              {status.key ? t('googleDelegation.key.replace') : t('googleDelegation.key.import')}
            </Button>
            {status.key && (
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => {
                  if (window.confirm(t('googleDelegation.key.removeConfirm'))) {
                    void run(() => api.googleIdentity.removeKey(), 'googleDelegation.key.removed');
                  }
                }}
              >
                {t('googleDelegation.key.remove')}
              </Button>
            )}
          </div>
          <p className="text-xs text-muted-foreground">{t('googleDelegation.key.hint')}</p>
        </div>

        <div className="rounded-lg border border-border bg-muted/40 p-3">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium">{t('googleDelegation.scopes')}</span>
            <Button variant="outline" size="sm" onClick={() => void copy('scopes', status.required_scopes.join(','))}>
              {copied === 'scopes' ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
              {copied === 'scopes' ? t('common.copied') : t('common.copy')}
            </Button>
          </div>
          <p className="mt-1.5 text-xs text-muted-foreground">{status.admin_console_path}</p>
          <p className="mt-1.5 break-all font-mono text-[10px] leading-4 text-muted-foreground">
            {status.required_scopes.join(',')}
          </p>
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="google-delegation-domains">
            {t('googleDelegation.domains')}
          </label>
          <div className="flex gap-2">
            <Input
              id="google-delegation-domains"
              value={domains}
              onChange={(e) => setDomains(e.target.value)}
              placeholder="customer.com"
            />
            <Button
              variant="outline"
              disabled={busy}
              onClick={() =>
                void run(
                  () =>
                    api.googleIdentity.setSettings({
                      allowed_subject_domains: domains
                        .split(/[,\s]+/)
                        .map((d) => d.trim())
                        .filter(Boolean),
                    }),
                  'googleDelegation.domainsSaved',
                )
              }
            >
              {t('common.save')}
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            {status.home_subject
              ? t('googleDelegation.domainsHintSubject', { subject: status.home_subject })
              : t('googleDelegation.domainsHint')}
          </p>
          {status.settings_error && <p className="text-xs text-warning">{status.settings_error}</p>}
        </div>

        <Separator />

        <div className="space-y-2">
          <p className="text-sm font-medium">{t('googleDelegation.table')}</p>
          {delegates.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t('googleDelegation.tableEmpty')}</p>
          ) : (
            <ul className="divide-y divide-border text-sm">
              {delegates.map((a) => (
                <li key={a.agent_id} className="flex flex-wrap items-center gap-2 py-2">
                  <button
                    type="button"
                    className="font-medium underline-offset-2 hover:underline"
                    onClick={() => navigate(`/agents/${a.agent_id}/connections`)}
                  >
                    {a.agent_id}
                  </button>
                  <span className="font-mono text-xs text-muted-foreground">{a.email}</span>
                  {a.state === 'ready' ? (
                    <Badge className="bg-success/15 text-success">{t('agentConnections.state.ready')}</Badge>
                  ) : (
                    <Badge className="bg-warning/15 text-warning">{t('agentConnections.state.blocked')}</Badge>
                  )}
                  {a.message && <span className="w-full text-xs text-warning">{a.message}</span>}
                </li>
              ))}
            </ul>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
