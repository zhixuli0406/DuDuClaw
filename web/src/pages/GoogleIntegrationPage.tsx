import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Mail,
  Calendar,
  Search,
  FileText,
  Video,
  CheckCircle2,
  Table2,
  Plus,
  AlertTriangle,
  Loader2,
} from 'lucide-react';
import { IntegrationConnectPanel } from '@/components/IntegrationConnectPanel';
import { GoogleCredentialPaths } from '@/components/GoogleCredentialPaths';
import { Card, CardContent, Button, Badge } from '@/components/mds';
import { api } from '@/lib/api';
import { toast } from '@/lib/toast';

const GOOGLE_CREDENTIALS_URL = 'https://console.cloud.google.com/apis/credentials';

/**
 * Two setup steps Google's console requires before an OAuth client can work at
 * all, inserted between "open the console" and "create the client". Leaving
 * them out of the UI (they were only in the written guide) guaranteed a dead
 * end: without the APIs enabled every call 403s, and without a consent screen
 * — plus the operator's own address as a test user while the app is in Testing
 * — Google blocks the authorization before it reaches DuDuClaw.
 */
const GOOGLE_EXTRA_STEPS = ['google.setup.enableApis', 'google.setup.consentScreen'] as const;

/** Agent capabilities this integration unlocks — user-facing copy only. */
const CAPABILITIES = [
  { icon: Search, id: 'google.cap.search' },
  { icon: Mail, id: 'google.cap.read' },
  { icon: FileText, id: 'google.cap.draft' },
  { icon: Calendar, id: 'google.cap.calendar' },
  { icon: Video, id: 'google.cap.meet' },
  { icon: Table2, id: 'google.cap.sheetsRead' },
  { icon: Plus, id: 'google.cap.sheetsAppend' },
  { icon: CheckCircle2, id: 'google.cap.status' },
] as const;

/**
 * GoogleIntegrationPage — Google Workspace setup, in three stacked surfaces:
 *
 *  1. The master-gate banner (G6). `config.toml [integrations]
 *     google_workspace` defaults to **false**, while this tab has been
 *     unconditionally visible since v1.48. That combination is the dead end
 *     the 2026-09 feature audit flagged: the page looks ready, the credential
 *     test goes green, and every agent's Google tool call still comes back
 *     refused. The banner reads the real backend flag and offers to open it,
 *     instead of leaving the operator to discover `config.toml`.
 *  2. {@link IntegrationConnectPanel} — the OAuth connect flow (the path that
 *     needs the customer's own Google Cloud OAuth client).
 *  3. {@link GoogleCredentialPaths} — the two alternatives that need no OAuth
 *     client at all: service-account domain-wide delegation (Workspace domains)
 *     and the Apps Script bridge (works for personal @gmail.com too).
 *
 * Google scope badges strip the long googleapis URL prefix for readability.
 */
export function GoogleIntegrationPage() {
  const intl = useIntl();
  const t = (id: string, defaultMessage: string) => intl.formatMessage({ id, defaultMessage });

  // `null` = not loaded yet (or unreadable): the banner stays hidden rather
  // than guessing, so a transient RPC failure never claims the integration is
  // off when it is on.
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const status = await api.googleCredentials.get();
      setEnabled(status.integration_enabled);
    } catch {
      setEnabled(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const setIntegration = async (next: boolean) => {
    setBusy(true);
    try {
      await api.googleCredentials.setIntegration(next);
      setEnabled(next);
      toast.success(
        next
          ? t('google.gate.enabled', 'Google 整合已啟用，工具現在會出現在 AI 員工面前。')
          : t('google.gate.disabled', 'Google 整合已關閉，AI 員工不再看得到這些工具。'),
      );
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      {enabled === false && (
        <Card data-testid="google-gate-off">
          <CardContent className="flex flex-col gap-3 py-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
              <div className="space-y-1">
                <p className="text-sm font-medium">
                  {t('google.gate.offTitle', 'Google 整合尚未啟用')}
                </p>
                <p className="text-sm text-muted-foreground">
                  {t(
                    'google.gate.offHint',
                    '先在下面完成一種連線方式，再按「啟用」，19 個 Google 工具才會出現在 AI 員工面前。在那之前工具呼叫都會被拒絕。',
                  )}
                </p>
              </div>
            </div>
            <Button onClick={() => void setIntegration(true)} disabled={busy}>
              {busy && <Loader2 className="size-4 animate-spin" />}
              {t('google.gate.enable', '啟用')}
            </Button>
          </CardContent>
        </Card>
      )}

      {enabled === true && (
        <Card data-testid="google-gate-on">
          <CardContent className="flex flex-col gap-3 py-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-2">
              <Badge className="bg-success/15 text-success">
                {t('google.gate.onTitle', '整合已啟用')}
              </Badge>
              <span className="text-sm text-muted-foreground">
                {t('google.gate.onHint', 'AI 員工看得到 Google 工具（仍須有可用的憑證）。')}
              </span>
            </div>
            <Button variant="outline" onClick={() => void setIntegration(false)} disabled={busy}>
              {busy && <Loader2 className="size-4 animate-spin" />}
              {t('google.gate.disable', '停用')}
            </Button>
          </CardContent>
        </Card>
      )}

      <IntegrationConnectPanel
        providerId="google"
        prefix="google"
        headerIcon={Mail}
        consoleUrl={GOOGLE_CREDENTIALS_URL}
        consoleLabel="Google Cloud Console"
        clientIdPlaceholder="xxxxxxxx.apps.googleusercontent.com"
        clientSecretPlaceholder="GOCSPX-..."
        capabilities={CAPABILITIES}
        extraSetupSteps={GOOGLE_EXTRA_STEPS}
        formatScope={(s) => s.replace('https://www.googleapis.com/auth/', '')}
      />
      <GoogleCredentialPaths />
    </div>
  );
}
