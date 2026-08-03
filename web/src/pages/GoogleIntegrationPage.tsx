import { Mail, Calendar, Search, FileText, Video, CheckCircle2, Table2, Plus } from 'lucide-react';
import { IntegrationConnectPanel } from '@/components/IntegrationConnectPanel';
import { GoogleCredentialPaths } from '@/components/GoogleCredentialPaths';

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
 * GoogleIntegrationPage — Google Workspace setup, in two stacked surfaces:
 *
 *  1. {@link IntegrationConnectPanel} — the OAuth connect flow (the path that
 *     needs the customer's own Google Cloud OAuth client).
 *  2. {@link GoogleCredentialPaths} — the two alternatives that need no OAuth
 *     client at all: service-account domain-wide delegation (Workspace domains)
 *     and the Apps Script bridge (works for personal @gmail.com too).
 *
 * Google scope badges strip the long googleapis URL prefix for readability.
 */
export function GoogleIntegrationPage() {
  return (
    <div className="space-y-4">
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
