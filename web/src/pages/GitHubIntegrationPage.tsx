import { Github, Search, FileSearch, GitPullRequest, MessageSquarePlus, CheckCircle2 } from 'lucide-react';
import { IntegrationConnectPanel } from '@/components/IntegrationConnectPanel';

const GITHUB_DEVELOPERS_URL = 'https://github.com/settings/developers';

/** Agent capabilities this integration unlocks — user-facing copy only. */
const CAPABILITIES = [
  { icon: Search, id: 'github.cap.search' },
  { icon: FileSearch, id: 'github.cap.issue' },
  { icon: GitPullRequest, id: 'github.cap.pr' },
  { icon: MessageSquarePlus, id: 'github.cap.comment' },
  { icon: CheckCircle2, id: 'github.cap.status' },
] as const;

/**
 * GitHubIntegrationPage — connect a GitHub account so AI employees can search,
 * read issues/PRs, and post comments. Rendered by the shared
 * {@link IntegrationConnectPanel}. Classic OAuth App tokens have no expiry, so
 * the connected view typically shows only the granted scopes (e.g. `repo`).
 */
export function GitHubIntegrationPage() {
  return (
    <IntegrationConnectPanel
      providerId="github"
      prefix="github"
      headerIcon={Github}
      consoleUrl={GITHUB_DEVELOPERS_URL}
      consoleLabel="GitHub · Developer settings"
      clientIdPlaceholder="Iv1.xxxxxxxxxxxxxxxx"
      clientSecretPlaceholder="ghp-oauth-client-secret"
      capabilities={CAPABILITIES}
    />
  );
}
