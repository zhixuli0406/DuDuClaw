import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { api } from '@/lib/api';
import { parseTomlSubset } from '@/lib/config-toml';
import { toast, formatError } from '@/lib/toast';

export type GoogleChatConnection = 'http' | 'pubsub';
export const CLOUD_SHELL_URL = 'https://shell.cloud.google.com/cloudshell/editor?cloudshell_git_repo=https%3A%2F%2Fgithub.com%2Fzhixuli0406%2FDuDuClaw&cloudshell_tutorial=scripts%2Fgooglechat%2Fcloudshell-tutorial.md&show=terminal';

/** Quote every variable argument: a copied command must remain data in a POSIX shell. */
export function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

export function bootstrapCommand(connection: GoogleChatConnection, endpoint: string, agentId?: string): string {
  return `duduclaw channels googlechat bootstrap --connection ${connection}`
    + (connection === 'http' ? ` --endpoint-url ${shellQuote(endpoint || 'https://YOUR-PUBLIC-HOST/webhook/googlechat')}` : '')
    + (agentId ? ` --agent ${shellQuote(agentId)}` : '');
}

export function publicEndpoint(base: string, path: string): string {
  try {
    const url = new URL(base);
    if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash) return '';
    return `${url.toString().replace(/\/$/, '')}${path}`;
  } catch { return ''; }
}

export function GoogleChatSetup({ connection, onConnectionChange, subscription, onSubscriptionChange, agentId, projectNumber, projectId, onRefresh }: {
  connection: GoogleChatConnection;
  onConnectionChange: (value: GoogleChatConnection) => void;
  subscription: string;
  onSubscriptionChange: (value: string) => void;
  agentId?: string;
  projectNumber?: string | null;
  projectId?: string | null;
  onRefresh?: () => Promise<void>;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `googlechat.setup.${id}` });
  const [endpoint, setEndpoint] = useState('');
  const [storedProject, setStoredProject] = useState('');
  const [email, setEmail] = useState('');
  const [loading, setLoading] = useState(false);
  const path = agentId ? `/webhook/googlechat/agents/${encodeURIComponent(agentId)}` : '/webhook/googlechat';
  const load = async () => {
    setLoading(true);
    try {
      const result = await api.system.config();
      const config = parseTomlSubset(result.config ?? '');
      const base = config.dashboard?.public_url;
      setEndpoint(publicEndpoint(typeof base === 'string' ? base : window.location.origin, path));
      if (!agentId) {
        const project = config.channels?.googlechat_project_number;
        setStoredProject(typeof project === 'string' ? project : '');
        const savedConnection = config.channels?.googlechat_connection;
        if (savedConnection === 'http' || savedConnection === 'pubsub') onConnectionChange(savedConnection);
        const savedSubscription = config.channels?.googlechat_pubsub_subscription;
        if (typeof savedSubscription === 'string') onSubscriptionChange(savedSubscription);
      }
      await onRefresh?.();
    } catch (error) {
      toast.error(formatError(error));
    } finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, [agentId]); // Load once per app; preserve in-progress edits.
  const project = projectNumber || storedProject;
  const subscriptionProject = /^projects\/([^/]+)\/subscriptions\/[^/]+$/.exec(subscription)?.[1];
  // Only the bootstrap's subscription has a known default topic. Custom
  // subscriptions may point elsewhere; never invent their topic from the project.
  const topic = /^projects\/[^/]+\/subscriptions\/duduclaw-chat-events-sub$/.test(subscription)
    ? `projects/${subscriptionProject}/topics/duduclaw-chat-events` : '';
  const command = bootstrapCommand(connection, endpoint || `https://YOUR-PUBLIC-HOST${path}`, agentId);
  const copy = async (value: string) => {
    try { await navigator.clipboard.writeText(value); toast.success(t('copied')); }
    catch { toast.error(t('copyFailed')); }
  };
  const appName = Array.from(agentId ? `DuDuClaw - ${agentId}` : 'DuDuClaw').slice(0, 25).join('');
  const values = [
    t('disableAddon'),
    `App name: ${appName}`,
    'Avatar URL: ',
    'Description: DuDuClaw AI assistant',
    'Interactive features: On',
    'Functionality: 1:1 messages and spaces',
    ...(connection === 'http'
      ? [`HTTP endpoint URL: ${endpoint || t('endpointNeeded')}`, 'Authentication Audience: Project number', `Project number: ${project ?? ''}`]
      : [`Cloud Pub/Sub topic: ${topic || t('topicFromScript')}`]),
    `Visibility: ${email || t('ownEmail')}`,
  ].join('\n');
  return <div className="space-y-3 rounded-lg border border-border p-3">
    <p className="text-sm font-medium">{t('title')}</p>
    <label className="block space-y-1 text-sm">
      <span>{t('connection')}</span>
      <select className="block w-full rounded-md border border-border bg-background p-2" value={connection} onChange={e => onConnectionChange(e.target.value as GoogleChatConnection)}>
        <option value="http">HTTP</option><option value="pubsub">Pub/Sub</option>
      </select>
    </label>
    <p className="text-xs text-muted-foreground">{t(connection === 'pubsub' ? 'pubsubHint' : 'httpHint')}</p>
    {connection === 'http' && <Input aria-label={t('endpoint')} placeholder={`https://YOUR-PUBLIC-HOST${path}`} value={endpoint} onChange={e => setEndpoint(e.target.value)} />}
    {connection === 'pubsub' && <Input aria-label={t('subscription')} placeholder="projects/PROJECT_ID/subscriptions/duduclaw-chat-events-sub" value={subscription} onChange={e => onSubscriptionChange(e.target.value)} />}
    <p className="text-xs text-muted-foreground">{t('dedicatedProject')}</p>
    <p className="text-xs text-muted-foreground">{t('disableAddon')}</p>
    <p className="text-xs text-muted-foreground">{t('localHint')}</p>
    <code className="block break-all rounded-md bg-secondary p-2 text-xs">{command}</code>
    <div className="flex flex-wrap gap-2">
      <Button size="sm" variant="outline" onClick={() => void copy(command)}>{t('copyCommand')}</Button>
      <a className="inline-flex items-center rounded-md border border-border px-3 py-1.5 text-sm" href={CLOUD_SHELL_URL} target="_blank" rel="noreferrer">{t('cloudShell')}</a>
      <Button size="sm" variant="outline" disabled={loading} onClick={() => void load()}>{t('refresh')}</Button>
    </div>
    <p className="text-xs text-muted-foreground">{t('cloudHint')}</p>
    {project && <div className="space-y-2 border-t border-border pt-3">
      <a className="text-sm text-primary underline" target="_blank" rel="noreferrer" href={`https://console.cloud.google.com/apis/api/chat.googleapis.com/hangouts-chat?project=${encodeURIComponent(projectId || subscriptionProject || project)}`}>{t('openConfig')}</a>
      <p className="text-xs text-muted-foreground">{t('configHint')}</p>
      <Input type="email" value={email} onChange={e => setEmail(e.target.value)} aria-label={t('visibility')} placeholder={t('ownEmail')} />
      <pre className="whitespace-pre-wrap break-all rounded-md bg-secondary p-2 text-xs">{values}</pre>
      <Button size="sm" variant="outline" onClick={() => void copy(values)}>{t('copyValues')}</Button>
    </div>}
  </div>;
}
