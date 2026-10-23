import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { ExternalLink, Trash2 } from 'lucide-react';
import { Button, Input, SettingsCard, SettingsSection, Textarea } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { api, type AgentOwnChannels, type AgentOwnChannelState } from '@/lib/api';
import { formatError, toast } from '@/lib/toast';
import { cn } from '@/lib/utils';

const DOCS_BASE = 'https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides';

/** The how-to guide in the viewer's language. */
export function guideUrl(locale: string): string {
  if (locale.startsWith('zh')) return `${DOCS_BASE}/zh-TW/per-employee-channels.md`;
  if (locale.startsWith('ja')) return `${DOCS_BASE}/ja-JP/per-employee-channels.md`;
  return `${DOCS_BASE}/per-employee-channels.md`;
}

const DOT: Record<AgentOwnChannelState, string> = {
  connected: 'bg-success',
  connecting: 'bg-warning',
  error: 'bg-destructive',
  not_configured: 'bg-muted-foreground/40',
};

type Removing = 'telegram' | 'googlechat' | null;

/**
 * 「專屬通道」 section of the employee page: this employee's own Telegram bot
 * and own Google Chat app. Hidden when the viewer cannot read it (the
 * gateway answers only the employee's owner or an admin). Controls are shown
 * only where the gateway says the viewer may change them.
 */
export function AgentOwnChannelsSection({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const t = useCallback(
    (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v),
    [intl],
  );
  const [data, setData] = useState<AgentOwnChannels | null>(null);
  const [hidden, setHidden] = useState(false);
  const [removing, setRemoving] = useState<Removing>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const r = await api.channels.agentStatus(agentId);
      // An older gateway (or an unexpected answer) hides the section.
      if (r && typeof r === 'object' && r.telegram && r.googlechat) setData(r);
      else setHidden(true);
    } catch {
      setHidden(true);
    }
  }, [agentId]);
  useEffect(() => {
    void load();
  }, [load]);

  const remove = async () => {
    if (!removing) return;
    setBusy(true);
    try {
      if (removing === 'telegram') await api.channels.agentTelegramRemove(agentId);
      else await api.channels.agentGoogleChatRemove(agentId);
      toast.success(t('agentChannels.removed'));
      setRemoving(null);
      await load();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
    }
  };

  if (hidden || !data) return null;
  return (
    <SettingsSection title={t('agentChannels.title')} description={t('agentChannels.desc')}>
      <TelegramCard data={data.telegram} agentId={agentId} onChanged={load} onRemove={() => setRemoving('telegram')} />
      <GoogleChatCard data={data.googlechat} agentId={agentId} onChanged={load} onRemove={() => setRemoving('googlechat')} />
      <a
        className="inline-flex items-center gap-1 text-xs text-primary underline-offset-4 hover:underline"
        href={guideUrl(intl.locale)}
        target="_blank"
        rel="noreferrer"
      >
        {t('agentChannels.guide')}
        <ExternalLink className="size-3" />
      </a>
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        onConfirm={() => void remove()}
        title={t('agentChannels.remove.title')}
        message={t(removing === 'googlechat' ? 'agentChannels.remove.googlechat' : 'agentChannels.remove.telegram')}
        confirmLabel={t('agentChannels.remove')}
        busy={busy}
      />
    </SettingsSection>
  );
}

function StatusLine({ state, children }: { state: AgentOwnChannelState; children: ReactNode }) {
  return (
    <div className="flex min-w-0 items-start gap-2 text-sm">
      <span className={cn('mt-1.5 inline-block size-2 shrink-0 rounded-full', DOT[state])} aria-hidden />
      <div className="min-w-0 break-words">{children}</div>
    </div>
  );
}

function Steps({ items }: { items: string[] }) {
  return (
    <ol className="list-decimal space-y-1 pl-5 text-xs text-muted-foreground">
      {items.map((s) => (
        <li key={s}>{s}</li>
      ))}
    </ol>
  );
}

function TelegramCard({
  data,
  agentId,
  onChanged,
  onRemove,
}: {
  data: AgentOwnChannels['telegram'];
  agentId: string;
  onChanged: () => Promise<void>;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [token, setToken] = useState('');
  const [saving, setSaving] = useState(false);

  const save = async () => {
    if (!token.trim()) return;
    setSaving(true);
    try {
      const r = await api.channels.agentTelegramSet(agentId, token.trim());
      setToken('');
      if (r.hot_started) toast.success(t('agentChannels.telegram.saved', { bot: r.bot_username ? `@${r.bot_username}` : '' }));
      else toast.error(t('agentChannels.telegram.savedNotStarted'));
      await onChanged();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const status = (() => {
    switch (data.state) {
      case 'connected':
        return data.bot_username
          ? t('agentChannels.telegram.connected', { bot: `@${data.bot_username}` })
          : t('agentChannels.state.connected');
      case 'connecting':
        return t('agentChannels.state.connecting');
      case 'error':
        return t('agentChannels.state.error', { error: data.error ?? '' });
      default:
        return data.shared_bot_configured
          ? t('agentChannels.telegram.usesShared')
          : t('agentChannels.telegram.none');
    }
  })();

  return (
    <SettingsCard>
      <div className="space-y-3 px-1 py-2.5">
        <div className="flex items-center justify-between gap-2">
          <span className="text-sm font-medium">Telegram</span>
          {data.configured && data.can_edit && (
            <Button variant="ghost" size="sm" onClick={onRemove}>
              <Trash2 className="size-3.5" />
              <span className="hidden sm:inline">{t('agentChannels.remove')}</span>
            </Button>
          )}
        </div>
        <StatusLine state={data.state}>{status}</StatusLine>
        {data.can_edit ? (
          <>
            <Steps items={[t('agentChannels.telegram.step1'), t('agentChannels.telegram.step2'), t('agentChannels.telegram.step3')]} />
            <div className="flex flex-col gap-2 sm:flex-row">
              <Input
                type="password"
                autoComplete="off"
                value={token}
                placeholder={t('agentChannels.telegram.placeholder')}
                aria-label={t('agentChannels.telegram.tokenLabel')}
                onChange={(e) => setToken(e.target.value)}
              />
              <Button onClick={() => void save()} disabled={saving || !token.trim()}>
                {data.configured ? t('agentChannels.replace') : t('agentChannels.connect')}
              </Button>
            </div>
          </>
        ) : (
          <p className="text-xs text-muted-foreground">{t('agentChannels.ownerOnly')}</p>
        )}
      </div>
    </SettingsCard>
  );
}

function GoogleChatCard({
  data,
  agentId,
  onChanged,
  onRemove,
}: {
  data: AgentOwnChannels['googlechat'];
  agentId: string;
  onChanged: () => Promise<void>;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [project, setProject] = useState(data.project_number ?? '');
  const [keyJson, setKeyJson] = useState('');
  const [saving, setSaving] = useState(false);
  useEffect(() => setProject(data.project_number ?? ''), [data.project_number]);
  const endpoint = `${window.location.origin}${data.endpoint_path}`;

  const save = async () => {
    if (!project.trim()) return;
    setSaving(true);
    try {
      const r = await api.channels.agentGoogleChatSet(agentId, project.trim(), keyJson);
      setKeyJson('');
      if (r.verified) toast.success(t('agentChannels.googlechat.saved'));
      else toast.error(t('agentChannels.googlechat.savedNotVerified', { error: r.error ?? '' }));
      await onChanged();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const status = (() => {
    switch (data.state) {
      case 'connected':
        return t('agentChannels.googlechat.connected', { account: data.service_account_email ?? '' });
      case 'connecting':
        return t('agentChannels.state.connecting');
      case 'error':
        return t('agentChannels.state.error', { error: data.error ?? '' });
      default:
        return data.shared_app_configured
          ? t('agentChannels.googlechat.usesShared')
          : t('agentChannels.googlechat.none');
    }
  })();

  return (
    <SettingsCard>
      <div className="space-y-3 px-1 py-2.5">
        <div className="flex items-center justify-between gap-2">
          <span className="text-sm font-medium">Google Chat</span>
          {data.configured && data.can_edit && (
            <Button variant="ghost" size="sm" onClick={onRemove}>
              <Trash2 className="size-3.5" />
              <span className="hidden sm:inline">{t('agentChannels.remove')}</span>
            </Button>
          )}
        </div>
        <StatusLine state={data.state}>{status}</StatusLine>
        {data.configured && data.project_number && (
          <p className="text-xs text-muted-foreground">
            {t('agentChannels.googlechat.projectShown', { project: data.project_number })}
          </p>
        )}
        <div className="space-y-1">
          <p className="text-xs font-medium">{t('agentChannels.googlechat.endpoint')}</p>
          <code className="block break-all rounded-md bg-secondary px-2 py-1 text-xs">{endpoint}</code>
          <p className="text-xs text-muted-foreground">{t('agentChannels.googlechat.endpointHint')}</p>
        </div>
        {data.can_edit ? (
          <>
            <Steps
              items={[
                t('agentChannels.googlechat.step1'),
                t('agentChannels.googlechat.step2'),
                t('agentChannels.googlechat.step3'),
                t('agentChannels.googlechat.step4'),
              ]}
            />
            <Input
              value={project}
              inputMode="numeric"
              placeholder={t('agentChannels.googlechat.projectPlaceholder')}
              aria-label={t('agentChannels.googlechat.projectLabel')}
              onChange={(e) => setProject(e.target.value)}
            />
            <Textarea
              value={keyJson}
              rows={4}
              spellCheck={false}
              placeholder={t(data.configured ? 'agentChannels.googlechat.keyKeep' : 'agentChannels.googlechat.keyPlaceholder')}
              aria-label={t('agentChannels.googlechat.keyLabel')}
              onChange={(e) => setKeyJson(e.target.value)}
            />
            <Button onClick={() => void save()} disabled={saving || !project.trim() || (!data.configured && !keyJson.trim())}>
              {data.configured ? t('agentChannels.update') : t('agentChannels.connect')}
            </Button>
          </>
        ) : (
          <p className="text-xs text-muted-foreground">{t('agentChannels.googlechat.adminOnly')}</p>
        )}
      </div>
    </SettingsCard>
  );
}
