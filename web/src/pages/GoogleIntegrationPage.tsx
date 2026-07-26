import { Mail, Calendar, Search, FileText, Video, CheckCircle2, Table2, Plus } from 'lucide-react';
import { IntegrationConnectPanel } from '@/components/IntegrationConnectPanel';

const GOOGLE_CREDENTIALS_URL = 'https://console.cloud.google.com/apis/credentials';

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
 * GoogleIntegrationPage — Gmail + Calendar + Sheets one-click setup, rendered by
 * the shared {@link IntegrationConnectPanel}. Google scope badges strip the long
 * googleapis URL prefix for readability.
 */
export function GoogleIntegrationPage() {
  return (
    <IntegrationConnectPanel
      providerId="google"
      prefix="google"
      headerIcon={Mail}
      consoleUrl={GOOGLE_CREDENTIALS_URL}
      consoleLabel="Google Cloud Console"
      clientIdPlaceholder="xxxxxxxx.apps.googleusercontent.com"
      clientSecretPlaceholder="GOCSPX-..."
      capabilities={CAPABILITIES}
      formatScope={(s) => s.replace('https://www.googleapis.com/auth/', '')}
    />
  );
}
