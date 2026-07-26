import { FileText, Search, BookOpen, FilePlus2, CheckCircle2 } from 'lucide-react';
import { IntegrationConnectPanel } from '@/components/IntegrationConnectPanel';

const NOTION_INTEGRATIONS_URL = 'https://www.notion.so/my-integrations';

/** Agent capabilities this integration unlocks — user-facing copy only. */
const CAPABILITIES = [
  { icon: Search, id: 'notion.cap.search' },
  { icon: BookOpen, id: 'notion.cap.read' },
  { icon: FilePlus2, id: 'notion.cap.append' },
  { icon: CheckCircle2, id: 'notion.cap.status' },
] as const;

/**
 * NotionIntegrationPage — connect a Notion workspace so AI employees can search
 * and read pages and append notes. Rendered by the shared
 * {@link IntegrationConnectPanel}. Notion tokens are long-lived (no expiry) and
 * carry no scopes, so the connected view shows neither an expiry nor scope
 * badges.
 */
export function NotionIntegrationPage() {
  return (
    <IntegrationConnectPanel
      providerId="notion"
      prefix="notion"
      headerIcon={FileText}
      consoleUrl={NOTION_INTEGRATIONS_URL}
      consoleLabel="Notion · My integrations"
      clientIdPlaceholder="2xxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
      clientSecretPlaceholder="secret_..."
      capabilities={CAPABILITIES}
    />
  );
}
