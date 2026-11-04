import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { api } from '@/lib/api';
import { parseTomlSubset } from '@/lib/config-toml';
import { toast, formatError } from '@/lib/toast';

export const CLOUD_SHELL_URL = 'https://shell.cloud.google.com/cloudshell/editor?cloudshell_git_repo=https%3A%2F%2Fgithub.com%2Fzhixuli0406%2FDuDuClaw&cloudshell_tutorial=scripts%2Fgooglechat%2Fcloudshell-tutorial.md&show=terminal';

/** Quote every variable argument: a copied command must remain data in a POSIX shell. */
export function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

export function bootstrapCommand(endpoint: string, agentId?: string): string {
  return `duduclaw channels googlechat bootstrap --endpoint-url ${shellQuote(endpoint || 'https://YOUR-PUBLIC-HOST/webhook/googlechat')}`
    + (agentId ? ` --agent ${shellQuote(agentId)}` : '');
}

export function publicEndpoint(base: string, path: string): string {
  try {
    const url = new URL(base);
    if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash) return '';
    return `${url.toString().replace(/\/$/, '')}${path}`;
  } catch { return ''; }
}

export function GoogleChatSetup({ agentId, projectNumber, projectId, onRefresh }: {
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
      }
      await onRefresh?.();
    } catch (error) {
      toast.error(formatError(error));
    } finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, [agentId]); // Load once per app; preserve in-progress edits.
  const project = projectNumber || storedProject;
  const command = bootstrapCommand(endpoint || `https://YOUR-PUBLIC-HOST${path}`, agentId);
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
    `HTTP endpoint URL: ${endpoint || t('endpointNeeded')}`,
    'Authentication Audience: Project number',
    `Project number: ${project ?? ''}`,
    `Visibility: ${email || t('ownEmail')}`,
  ].join('\n');
  return <div className="space-y-3 rounded-lg border border-border p-3">
    <p className="text-sm font-medium">{t('title')}</p>
    <p className="text-xs text-muted-foreground">{t('httpHint')}</p>
    <Input aria-label={t('endpoint')} placeholder={`https://YOUR-PUBLIC-HOST${path}`} value={endpoint} onChange={e => setEndpoint(e.target.value)} />
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
      <a className="text-sm text-primary underline" target="_blank" rel="noreferrer" href={`https://console.cloud.google.com/apis/api/chat.googleapis.com/hangouts-chat?project=${encodeURIComponent(projectId || project)}`}>{t('openConfig')}</a>
      <p className="text-xs text-muted-foreground">{t('configHint')}</p>
      <Input type="email" value={email} onChange={e => setEmail(e.target.value)} aria-label={t('visibility')} placeholder={t('ownEmail')} />
      <pre className="whitespace-pre-wrap break-all rounded-md bg-secondary p-2 text-xs">{values}</pre>
      <Button size="sm" variant="outline" onClick={() => void copy(values)}>{t('copyValues')}</Button>
    </div>}
  </div>;
}
