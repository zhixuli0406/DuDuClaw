import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type ChannelStatus, type AgentInfo } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { toast, formatError } from '@/lib/toast';
import { useConnectionStore } from '@/stores/connection-store';
import { Dialog, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { ConfirmDialog } from '@/components/settings/controls';
import {
  Page,
  PageHeader,
  Card,
  Badge,
  Button,
  EmptyState,
  Field,
  Mono,
  controlClass,
} from '@/components/ui';
import {
  Radio,
  Plus,
  TestTube,
  Trash2,
  CheckCircle,
  XCircle,
  Pencil,
  AlertTriangle,
  X,
  Loader2,
} from 'lucide-react';

const channelMeta: Record<
  string,
  { color: string; bg: string; darkBg: string }
> = {
  line: {
    color: 'text-green-600 dark:text-green-400',
    bg: 'bg-green-100',
    darkBg: 'dark:bg-green-900/30',
  },
  telegram: {
    color: 'text-blue-600 dark:text-blue-400',
    bg: 'bg-blue-100',
    darkBg: 'dark:bg-blue-900/30',
  },
  discord: {
    color: 'text-purple-600 dark:text-purple-400',
    bg: 'bg-purple-100',
    darkBg: 'dark:bg-purple-900/30',
  },
  slack: {
    color: 'text-rose-600 dark:text-rose-400',
    bg: 'bg-rose-100',
    darkBg: 'dark:bg-rose-900/30',
  },
  whatsapp: {
    color: 'text-emerald-600 dark:text-emerald-400',
    bg: 'bg-emerald-100',
    darkBg: 'dark:bg-emerald-900/30',
  },
  feishu: {
    color: 'text-sky-600 dark:text-sky-400',
    bg: 'bg-sky-100',
    darkBg: 'dark:bg-sky-900/30',
  },
};

function getChannelPlatform(name: string): string {
  return name.split(':')[0].toLowerCase();
}

function getChannelStyle(name: string) {
  const key = getChannelPlatform(name);
  return (
    channelMeta[key] ?? {
      color: 'text-stone-600 dark:text-stone-400',
      bg: 'bg-stone-100',
      darkBg: 'dark:bg-stone-800',
    }
  );
}

export function ChannelsPage() {
  const intl = useIntl();
  const connState = useConnectionStore((s) => s.state);
  const [channels, setChannels] = useState<ReadonlyArray<ChannelStatus>>([]);
  const [loading, setLoading] = useState(false);
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [editChannel, setEditChannel] = useState<string | null>(null);
  const [removeTarget, setRemoveTarget] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  const [toast, setToast] = useState<{ type: 'success' | 'error'; message: string } | null>(null);

  const toastTimerRef = useRef<ReturnType<typeof setTimeout>>(null);
  const showToast = useCallback((type: 'success' | 'error', message: string) => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    setToast({ type, message });
    toastTimerRef.current = setTimeout(() => setToast(null), type === 'error' ? 8000 : 4000);
  }, []);
  const dismissToast = useCallback(() => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    setToast(null);
  }, []);
  useEffect(() => {
    return () => { if (toastTimerRef.current) clearTimeout(toastTimerRef.current); };
  }, []);

  const fetchChannels = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.channels.status();
      setChannels(result?.channels ?? []);
    } catch {
      showToast('error', intl.formatMessage({ id: 'channels.loadFailed' }));
    } finally {
      setLoading(false);
    }
  }, []);

  // Wait for WebSocket to be authenticated before fetching
  useEffect(() => {
    if (connState === 'authenticated') {
      fetchChannels();
    }
  }, [connState, fetchChannels]);

  // Subscribe to real-time channel status changes
  useEffect(() => {
    const unsubscribe = client.subscribe('channels.status_changed', (payload) => {
      const update = payload as { name: string; connected: boolean; last_connected?: string; error?: string | null };
      setChannels((prev) => {
        const exists = prev.some((ch) => ch.name === update.name);
        if (exists) {
          return prev.map((ch) =>
            ch.name === update.name
              ? { ...ch, connected: update.connected, last_connected: update.last_connected, error: update.error ?? undefined }
              : ch
          );
        }
        // New channel appeared — add it
        return [...prev, {
          name: update.name,
          connected: update.connected,
          last_connected: update.last_connected,
          error: update.error ?? undefined,
        }];
      });

      // Show toast for notable status changes
      if (update.error && update.error !== 'connecting' && update.error !== 'reconnecting') {
        showToast('error', `${update.name}: ${update.error}`);
      } else if (update.connected) {
        showToast('success', intl.formatMessage({ id: 'channels.connected.toast' }, { name: update.name }));
      }
    });
    return unsubscribe;
  }, [intl]);

  const handleTest = async (type: string) => {
    try {
      const result = await api.channels.test(type) as { success: boolean; message: string };
      showToast(result.success ? 'success' : 'error', result.message);
      await fetchChannels();
    } catch {
      showToast('error', intl.formatMessage({ id: 'channels.testFailed' }));
    }
  };

  const handleRemove = async (type: string) => {
    setRemoving(true);
    try {
      await api.channels.remove(type);
      showToast('success', intl.formatMessage({ id: 'channels.removed' }, { type }));
      await fetchChannels();
      setRemoveTarget(null);
    } catch (e) {
      showToast('error', intl.formatMessage({ id: 'channels.removeFailed' }, { error: String(e) }));
    } finally {
      setRemoving(false);
    }
  };

  return (
    <Page>
      <PageHeader
        icon={Radio}
        title={intl.formatMessage({ id: 'channels.title' })}
        subtitle={intl.formatMessage({ id: 'channels.subtitle' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setShowAddDialog(true)}>
            {intl.formatMessage({ id: 'channels.add' })}
          </Button>
        }
      />

      {/* Toast notification */}
      {toast && (
        <div className={cn(
          'flex items-start gap-3 rounded-control px-4 py-3 text-sm transition-all',
          toast.type === 'success'
            ? 'bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400'
            : 'bg-rose-50 text-rose-700 dark:bg-rose-900/20 dark:text-rose-400'
        )}>
          {toast.type === 'success' ? (
            <CheckCircle className="mt-0.5 h-4 w-4 shrink-0" />
          ) : (
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          )}
          <span className="flex-1">{toast.message}</span>
          <button
            onClick={dismissToast}
            className="shrink-0 rounded p-0.5 opacity-60 transition-opacity hover:opacity-100"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {channels.length === 0 && !loading ? (
        <Card padded={false}>
          <EmptyState
            icon={Radio}
            dudu="curious"
            title={intl.formatMessage({ id: 'channels.empty' })}
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {channels.map((channel) => {
            const style = getChannelStyle(channel.name);
            return (
              <Card key={channel.name} interactive>
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <div
                      className={cn(
                        'rounded-control p-2.5',
                        style.bg,
                        style.darkBg
                      )}
                    >
                      <Radio className={cn('h-5 w-5', style.color)} />
                    </div>
                    <div>
                      <h3 className="font-semibold capitalize text-stone-900 dark:text-stone-50">
                        {channel.name}
                      </h3>
                      {channel.last_connected && (
                        <Mono className="block text-xs text-stone-500 dark:text-stone-400">
                          {new Date(channel.last_connected).toLocaleString(
                            'zh-TW'
                          )}
                        </Mono>
                      )}
                    </div>
                  </div>

                  {/* Connection status */}
                  {channel.connected ? (
                    <Badge tone="success">
                      <CheckCircle className="h-3 w-3" />
                      {intl.formatMessage({ id: 'status.connected' })}
                    </Badge>
                  ) : channel.error === 'connecting' || channel.error === 'reconnecting' ? (
                    <Badge tone="warning">
                      <Loader2 className="h-3 w-3 animate-spin" />
                      {channel.error === 'reconnecting'
                        ? intl.formatMessage({ id: 'status.reconnecting' })
                        : intl.formatMessage({ id: 'status.connecting' })}
                    </Badge>
                  ) : (
                    <Badge tone="danger">
                      <XCircle className="h-3 w-3" />
                      {intl.formatMessage({ id: 'status.disconnected' })}
                    </Badge>
                  )}
                </div>

                {/* Error message — hide transitional states */}
                {channel.error && channel.error !== 'connecting' && channel.error !== 'reconnecting' && (
                  <div className="mt-3 flex items-start gap-2 rounded-control bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
                    <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
                    <span>{channel.error}</span>
                  </div>
                )}

                {/* Actions */}
                <div className="mt-4 flex gap-2 border-t border-[var(--panel-border)] pt-3">
                  <Button size="sm" variant="ghost" icon={TestTube} onClick={() => handleTest(channel.name)}>
                    {intl.formatMessage({ id: 'channels.test' })}
                  </Button>
                  <Button size="sm" variant="ghost" icon={Pencil} onClick={() => setEditChannel(channel.name)}>
                    {intl.formatMessage({ id: 'channels.edit' })}
                  </Button>
                  <Button size="sm" variant="ghost" icon={Trash2} onClick={() => setRemoveTarget(channel.name)} className="text-rose-600 hover:bg-rose-50 hover:text-rose-700 dark:text-rose-400 dark:hover:bg-rose-900/20">
                    {intl.formatMessage({ id: 'channels.remove' })}
                  </Button>
                </div>
              </Card>
            );
          })}
        </div>
      )}
      {/* Add Channel Dialog */}
      <AddChannelDialog
        open={showAddDialog}
        onClose={() => setShowAddDialog(false)}
        onCreated={fetchChannels}
      />

      {/* Edit Channel Dialog (re-uses add flow to replace token) */}
      <AddChannelDialog
        open={editChannel !== null}
        onClose={() => setEditChannel(null)}
        onCreated={() => { setEditChannel(null); fetchChannels(); }}
        fixedType={editChannel ?? undefined}
      />

      {/* Destructive remove confirmation (replaces window.confirm) */}
      <ConfirmDialog
        open={removeTarget !== null}
        onClose={() => setRemoveTarget(null)}
        onConfirm={() => { if (removeTarget) handleRemove(removeTarget); }}
        title={intl.formatMessage({ id: 'channels.remove.confirmTitle' })}
        message={removeTarget ? intl.formatMessage({ id: 'channels.confirmRemove' }, { type: removeTarget }) : ''}
        confirmLabel={intl.formatMessage({ id: 'channels.remove' })}
        busy={removing}
      />
    </Page>
  );
}

const SUPPORTS_PER_AGENT = ['discord', 'telegram', 'slack'];

function AddChannelDialog({ open, onClose, onCreated, fixedType }: { open: boolean; onClose: () => void; onCreated: () => void; fixedType?: string }) {
  const intl = useIntl();
  // Parse fixedType: "discord:lab-bot" → platform="discord", agent="lab-bot"
  const parsedPlatform = fixedType?.split(':')[0];
  const parsedAgent = fixedType?.includes(':') ? fixedType.split(':').slice(1).join(':') : undefined;

  const [channelType, setChannelType] = useState(parsedPlatform ?? fixedType ?? 'line');
  const [selectedAgent, setSelectedAgent] = useState(parsedAgent ?? '');
  const [agents, setAgents] = useState<AgentInfo[]>([]);

  useEffect(() => {
    if (fixedType) {
      setChannelType(parsedPlatform ?? fixedType);
      setSelectedAgent(parsedAgent ?? '');
    }
  }, [fixedType, parsedPlatform, parsedAgent]);

  useEffect(() => {
    if (open) {
      api.agents.list().then((r) => setAgents(r.agents ?? [])).catch((e) => {
        console.warn("[api]", e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
    }
  }, [open, intl]);

  const [token, setToken] = useState('');
  const [secret, setSecret] = useState('');
  // G.6 — extra per-platform tokens stored under config.* keys (write-only).
  const [waVerifyToken, setWaVerifyToken] = useState('');
  const [waAppSecret, setWaAppSecret] = useState('');
  const [feishuVerifyToken, setFeishuVerifyToken] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);

  const handleSubmit = async () => {
    if (!token.trim()) return;
    setSubmitting(true);
    try {
      const config: Record<string, string> = { token: token.trim() };
      if (secret.trim()) config.secret = secret.trim();
      // G.6 — extra global channel tokens; only sent when typed.
      if (channelType === 'whatsapp') {
        if (waVerifyToken.trim()) config.whatsapp_verify_token = waVerifyToken.trim();
        if (waAppSecret.trim()) config.whatsapp_app_secret = waAppSecret.trim();
      }
      if (channelType === 'feishu' && feishuVerifyToken.trim()) {
        config.feishu_verification_token = feishuVerifyToken.trim();
      }
      await api.channels.add(channelType, config, selectedAgent || undefined);
      onCreated();
      onClose();
      setToken('');
      setSecret('');
      setWaVerifyToken('');
      setWaAppSecret('');
      setFeishuVerifyToken('');
      setSelectedAgent('');
    } catch (e) {
      setAddError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const channelGuide: Record<string, { tokenLabel: string; secretLabel?: string; stepKeys: string[] }> = {
    telegram: {
      tokenLabel: 'Bot Token',
      stepKeys: [
        'channels.setup.telegram.step1',
        'channels.setup.telegram.step2',
        'channels.setup.telegram.step3',
        'channels.setup.telegram.note',
      ],
    },
    line: {
      tokenLabel: 'Channel Access Token',
      secretLabel: 'Channel Secret',
      stepKeys: [
        'channels.setup.line.step1',
        'channels.setup.line.step2',
        'channels.setup.line.step3',
        'channels.setup.line.step4',
        'channels.setup.line.step5',
        'channels.setup.line.note',
      ],
    },
    discord: {
      tokenLabel: 'Bot Token',
      stepKeys: [
        'channels.setup.discord.step1',
        'channels.setup.discord.step2',
        'channels.setup.discord.step3',
        'channels.setup.discord.intentWarning',
        'channels.setup.discord.intentRecommend',
        'channels.setup.discord.step4',
        'channels.setup.discord.step5',
        'channels.setup.discord.perm1',
        'channels.setup.discord.perm2',
        'channels.setup.discord.perm3',
        'channels.setup.discord.step6',
        'channels.setup.discord.reinviteTip',
      ],
    },
    slack: {
      tokenLabel: 'Bot User OAuth Token (xoxb-...)',
      secretLabel: 'App-Level Token (xapp-...)',
      stepKeys: [
        'channels.setup.slack.step1',
        'channels.setup.slack.step2',
        'channels.setup.slack.step3',
        'channels.setup.slack.step4',
        'channels.setup.slack.step5',
        'channels.setup.slack.note',
      ],
    },
    whatsapp: {
      tokenLabel: 'Access Token',
      secretLabel: 'Phone Number ID',
      stepKeys: [
        'channels.setup.whatsapp.step1',
        'channels.setup.whatsapp.step2',
        'channels.setup.whatsapp.step3',
        'channels.setup.whatsapp.step4',
        'channels.setup.whatsapp.step5',
        'channels.setup.whatsapp.step6',
        'channels.setup.whatsapp.note',
      ],
    },
    feishu: {
      tokenLabel: 'App ID',
      secretLabel: 'App Secret',
      stepKeys: [
        'channels.setup.feishu.step1',
        'channels.setup.feishu.step2',
        'channels.setup.feishu.step3',
        'channels.setup.feishu.step4',
        'channels.setup.feishu.step5',
        'channels.setup.feishu.step6',
      ],
    },
  };

  const guide = channelGuide[channelType] ?? { tokenLabel: 'Token', stepKeys: [] };
  const steps = guide.stepKeys.map((id) => intl.formatMessage({ id }));

  return (
    <Dialog open={open} onClose={onClose} title={fixedType ? intl.formatMessage({ id: 'channels.dialog.editTitle' }, { type: fixedType }) : intl.formatMessage({ id: 'channels.dialog.addTitle' })}>
      <div className="space-y-4">
        <Field label={intl.formatMessage({ id: 'channels.dialog.type' })}>
          <select value={channelType} onChange={(e) => setChannelType(e.target.value)} disabled={!!fixedType} className={controlClass}>
            <option value="telegram">Telegram</option>
            <option value="line">LINE</option>
            <option value="discord">Discord</option>
            <option value="slack">Slack</option>
            <option value="whatsapp">WhatsApp</option>
            <option value="feishu">Feishu</option>
          </select>
        </Field>

        {SUPPORTS_PER_AGENT.includes(channelType) && agents.length > 0 && (
          <Field
            label={intl.formatMessage({ id: 'channels.dialog.assignAgent' })}
            help={intl.formatMessage({ id: 'channels.dialog.assignAgentHint' })}
          >
            <select value={selectedAgent} onChange={(e) => setSelectedAgent(e.target.value)} className={controlClass}>
              <option value="">{intl.formatMessage({ id: 'channels.dialog.global' })}</option>
              {agents.map((a) => (
                <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
              ))}
            </select>
          </Field>
        )}

        {/* Setup guide */}
        <div className="rounded-control bg-amber-50 p-3 text-xs text-amber-800 dark:bg-amber-900/20 dark:text-amber-300">
          <p className="mb-1 font-medium">{intl.formatMessage({ id: 'channels.dialog.setupGuide' })}</p>
          {steps.map((step, i) => (
            <p key={i} className={step.startsWith('⚠') ? 'font-semibold text-rose-600 dark:text-rose-400' : ''}>
              {step}
            </p>
          ))}
        </div>

        <Field label={guide.tokenLabel}>
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={intl.formatMessage({ id: 'channels.dialog.pastePlaceholder' }, { tokenLabel: guide.tokenLabel.toLowerCase() })}
            className={controlClass}
          />
        </Field>

        {guide.secretLabel && (
          <Field label={guide.secretLabel}>
            <input
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={guide.secretLabel}
              className={controlClass}
            />
          </Field>
        )}

        {/* G.6 — extra WhatsApp tokens (global) */}
        {channelType === 'whatsapp' && (
          <>
            <Field label="Verify Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
              <input type="password" value={waVerifyToken} onChange={(e) => setWaVerifyToken(e.target.value)} className={controlClass} autoComplete="off" />
            </Field>
            <Field label="App Secret" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
              <input type="password" value={waAppSecret} onChange={(e) => setWaAppSecret(e.target.value)} className={controlClass} autoComplete="off" />
            </Field>
          </>
        )}

        {/* G.6 — extra Feishu token (global) */}
        {channelType === 'feishu' && (
          <Field label="Verification Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
            <input type="password" value={feishuVerifyToken} onChange={(e) => setFeishuVerifyToken(e.target.value)} className={controlClass} autoComplete="off" />
          </Field>
        )}

        {addError && (
          <div className="flex items-start gap-2 rounded-control bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
            <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
            <span>{addError}</span>
          </div>
        )}

        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>{intl.formatMessage({ id: 'channels.dialog.cancel' })}</button>
          <button onClick={() => { setAddError(null); handleSubmit(); }} disabled={submitting || !token.trim()} className={buttonPrimary}>
            {submitting ? intl.formatMessage({ id: 'channels.dialog.adding' }) : intl.formatMessage({ id: 'channels.dialog.add' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
