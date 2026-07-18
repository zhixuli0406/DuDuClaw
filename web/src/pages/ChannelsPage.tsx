import { useEffect, useState, useCallback, useRef, useMemo, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import qrcode from 'qrcode-generator';
import { cn } from '@/lib/utils';
import { api, type ChannelStatus, type AgentInfo } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { toast, formatError } from '@/lib/toast';
import { useConnectionStore } from '@/stores/connection-store';
import { ConfirmDialog } from '@/components/settings/controls';
import {
  Button,
  Card,
  CardContent,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  Empty,
} from '@/components/mds';
import {
  Radio,
  Plus,
  TestTube,
  Trash2,
  CheckCircle,
  Pencil,
  AlertTriangle,
  X,
  Link2,
  Copy,
  Check,
  MoreHorizontal,
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
  // Slack / WhatsApp keep raw palette hues like the sibling platforms — these
  // are platform identity tints (brand colors), not status semantics.
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
  wecom: {
    color: 'text-cyan-600 dark:text-cyan-400',
    bg: 'bg-cyan-100',
    darkBg: 'dark:bg-cyan-900/30',
  },
  dingtalk: {
    color: 'text-indigo-600 dark:text-indigo-400',
    bg: 'bg-indigo-100',
    darkBg: 'dark:bg-indigo-900/30',
  },
  googlechat: {
    color: 'text-teal-600 dark:text-teal-400',
    bg: 'bg-teal-100',
    darkBg: 'dark:bg-teal-900/30',
  },
  teams: {
    color: 'text-violet-600 dark:text-violet-400',
    bg: 'bg-violet-100',
    darkBg: 'dark:bg-violet-900/30',
  },
};

/** Channel type picker options — value ⇒ human label (spec §4 Select). */
const CHANNEL_TYPES: ReadonlyArray<{ value: string; label: string }> = [
  { value: 'telegram', label: 'Telegram' },
  { value: 'line', label: 'LINE' },
  { value: 'discord', label: 'Discord' },
  { value: 'slack', label: 'Slack' },
  { value: 'whatsapp', label: 'WhatsApp' },
  { value: 'feishu', label: 'Feishu' },
  { value: 'wecom', label: 'WeCom (企業微信)' },
  { value: 'dingtalk', label: 'DingTalk (釘釘)' },
  { value: 'googlechat', label: 'Google Chat' },
  { value: 'teams', label: 'Microsoft Teams' },
];

function getChannelPlatform(name: string): string {
  return name.split(':')[0].toLowerCase();
}

function getChannelStyle(name: string) {
  const key = getChannelPlatform(name);
  return (
    channelMeta[key] ?? {
      color: 'text-muted-foreground',
      bg: 'bg-muted',
      darkBg: '',
    }
  );
}

/**
 * ChannelsPage (`/channels`) — the messaging-channel roster on the MDS surface.
 * A Radio-icon header with employee-bind + add actions, a card-list of channel
 * rows (icon tile · name · status dot · last-connected · kebab), and MDS Dialogs
 * for add/edit + the Telegram shared-bot bind flow. All `api.channels.*` calls
 * are unchanged; the Calm-Glass primitives are gone.
 */
export function ChannelsPage() {
  const intl = useIntl();
  const connState = useConnectionStore((s) => s.state);
  const [channels, setChannels] = useState<ReadonlyArray<ChannelStatus>>([]);
  const [loading, setLoading] = useState(false);
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [showBindDialog, setShowBindDialog] = useState(false);
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
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Radio className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'channels.title' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'channels.subtitle' })}</p>
          </div>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => setShowBindDialog(true)}>
            <Link2 />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'channels.bind.action' })}</span>
          </Button>
          <Button variant="brand" size="sm" onClick={() => setShowAddDialog(true)}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'channels.add' })}</span>
          </Button>
        </div>
      </div>

      {/* Toast notification */}
      {toast && (
        <div className={cn(
          'flex items-start gap-3 rounded-lg px-4 py-3 text-sm transition-all',
          toast.type === 'success'
            ? 'bg-success/10 text-success'
            : 'bg-destructive/10 text-destructive'
        )}>
          {toast.type === 'success' ? (
            <CheckCircle className="mt-0.5 size-4 shrink-0" />
          ) : (
            <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          )}
          <span className="flex-1">{toast.message}</span>
          <button
            onClick={dismissToast}
            className="shrink-0 rounded p-0.5 opacity-60 transition-opacity hover:opacity-100"
            aria-label={intl.formatMessage({ id: 'common.cancel' })}
          >
            <X className="size-3.5" />
          </button>
        </div>
      )}

      {channels.length === 0 && !loading ? (
        <Empty icon={Radio} title={intl.formatMessage({ id: 'channels.empty' })} />
      ) : (
        <div className="space-y-2">
          {channels.map((channel) => (
            <ChannelRow
              key={channel.name}
              channel={channel}
              onTest={() => handleTest(channel.name)}
              onEdit={() => setEditChannel(channel.name)}
              onRemove={() => setRemoveTarget(channel.name)}
            />
          ))}
        </div>
      )}

      {/* Add Channel Dialog */}
      <AddChannelDialog
        open={showAddDialog}
        onClose={() => setShowAddDialog(false)}
        onCreated={fetchChannels}
      />

      {/* WP9 — Telegram shared-bot employee bind link / QR */}
      <TelegramBindDialog
        open={showBindDialog}
        onClose={() => setShowBindDialog(false)}
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
    </div>
  );
}

/** One channel as an MDS Card row: icon tile · name · status dot · kebab. */
function ChannelRow({
  channel,
  onTest,
  onEdit,
  onRemove,
}: {
  channel: ChannelStatus;
  onTest: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const style = getChannelStyle(channel.name);
  const transitional = channel.error === 'connecting' || channel.error === 'reconnecting';

  const status = channel.connected
    ? { dot: 'bg-success', pulse: false, label: intl.formatMessage({ id: 'status.connected' }) }
    : transitional
      ? {
          dot: 'bg-warning',
          pulse: true,
          label: intl.formatMessage({
            id: channel.error === 'reconnecting' ? 'status.reconnecting' : 'status.connecting',
          }),
        }
      : { dot: 'bg-destructive', pulse: false, label: intl.formatMessage({ id: 'status.disconnected' }) };

  return (
    <Card data-size="sm">
      <CardContent className="space-y-2">
        <div className="flex items-center gap-3">
          <div className={cn('flex size-9 shrink-0 items-center justify-center rounded-lg', style.bg, style.darkBg)}>
            <Radio className={cn('size-5', style.color)} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h3 className="truncate text-sm font-medium capitalize text-foreground">{channel.name}</h3>
              <span className={cn('size-2 shrink-0 rounded-full', status.dot, status.pulse && 'animate-pulse')} />
              <span className="text-xs text-muted-foreground">{status.label}</span>
            </div>
            {channel.last_connected && (
              <p className="mt-0.5 font-mono text-xs text-muted-foreground">
                {new Date(channel.last_connected).toLocaleString('zh-TW')}
              </p>
            )}
          </div>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={intl.formatMessage({ id: 'common.more' })}
                />
              }
            >
              <MoreHorizontal />
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem onClick={onTest}>
                <TestTube />
                {intl.formatMessage({ id: 'channels.test' })}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={onEdit}>
                <Pencil />
                {intl.formatMessage({ id: 'channels.edit' })}
              </DropdownMenuItem>
              <DropdownMenuItem variant="destructive" onClick={onRemove}>
                <Trash2 />
                {intl.formatMessage({ id: 'channels.remove' })}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        {/* Error message — hide transitional states */}
        {channel.error && !transitional && (
          <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
            <AlertTriangle className="mt-0.5 size-3 shrink-0" />
            <span>{channel.error}</span>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

const SUPPORTS_PER_AGENT = ['discord', 'telegram', 'slack'];

/** Stacked label + control block used across the channel dialogs (spec §5.3). */
function DialogField({
  label,
  help,
  children,
}: {
  label: string;
  help?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground">{label}</label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

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
  const [teamsTenantId, setTeamsTenantId] = useState('');
  const [wecomAgentId, setWecomAgentId] = useState('');
  const [wecomCallbackToken, setWecomCallbackToken] = useState('');
  const [wecomAesKey, setWecomAesKey] = useState('');
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
      if (channelType === 'teams' && teamsTenantId.trim()) {
        config.teams_tenant_id = teamsTenantId.trim();
      }
      if (channelType === 'wecom') {
        if (wecomAgentId.trim()) config.wecom_agent_id = wecomAgentId.trim();
        if (wecomCallbackToken.trim()) config.wecom_callback_token = wecomCallbackToken.trim();
        if (wecomAesKey.trim()) config.wecom_encoding_aes_key = wecomAesKey.trim();
      }
      await api.channels.add(channelType, config, selectedAgent || undefined);
      onCreated();
      onClose();
      setToken('');
      setSecret('');
      setWaVerifyToken('');
      setWaAppSecret('');
      setFeishuVerifyToken('');
      setTeamsTenantId('');
      setWecomAgentId('');
      setWecomCallbackToken('');
      setWecomAesKey('');
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
    wecom: {
      tokenLabel: 'Corp Secret',
      secretLabel: 'Corp ID',
      stepKeys: [],
    },
    dingtalk: {
      tokenLabel: 'App Secret',
      secretLabel: 'App Key (Client ID)',
      stepKeys: [],
    },
    googlechat: {
      tokenLabel: 'Service Account JSON',
      secretLabel: 'Project Number',
      stepKeys: [
        'channels.setup.googlechat.step1',
        'channels.setup.googlechat.step2',
        'channels.setup.googlechat.step3',
        'channels.setup.googlechat.note',
      ],
    },
    teams: {
      tokenLabel: 'App Password',
      secretLabel: 'App ID',
      stepKeys: [
        'channels.setup.teams.step1',
        'channels.setup.teams.step2',
        'channels.setup.teams.step3',
        'channels.setup.teams.note',
      ],
    },
  };

  const guide = channelGuide[channelType] ?? { tokenLabel: 'Token', stepKeys: [] };
  const steps = guide.stepKeys.map((id) => intl.formatMessage({ id }));
  const typeLabel = CHANNEL_TYPES.find((c) => c.value === channelType)?.label ?? channelType;

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {fixedType
              ? intl.formatMessage({ id: 'channels.dialog.editTitle' }, { type: fixedType })
              : intl.formatMessage({ id: 'channels.dialog.addTitle' })}
          </DialogTitle>
        </DialogHeader>

        <div className="max-h-[60vh] space-y-4 overflow-y-auto">
          <DialogField label={intl.formatMessage({ id: 'channels.dialog.type' })}>
            <Select value={channelType} onValueChange={(v) => setChannelType(String(v))} disabled={!!fixedType}>
              <SelectTrigger className="w-full">
                <SelectValue>{typeLabel}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {CHANNEL_TYPES.map((c) => (
                  <SelectItem key={c.value} value={c.value}>{c.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>

          {SUPPORTS_PER_AGENT.includes(channelType) && agents.length > 0 && (
            <DialogField
              label={intl.formatMessage({ id: 'channels.dialog.assignAgent' })}
              help={intl.formatMessage({ id: 'channels.dialog.assignAgentHint' })}
            >
              <Select value={selectedAgent} onValueChange={(v) => setSelectedAgent(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>
                    {selectedAgent
                      ? agents.find((a) => a.name === selectedAgent)?.display_name || selectedAgent
                      : intl.formatMessage({ id: 'channels.dialog.global' })}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="">{intl.formatMessage({ id: 'channels.dialog.global' })}</SelectItem>
                  {agents.map((a) => (
                    <SelectItem key={a.name} value={a.name}>{a.display_name || a.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </DialogField>
          )}

          {/* Setup guide */}
          <div className="rounded-lg bg-warning/10 p-3 text-xs text-warning">
            <p className="mb-1 font-medium">{intl.formatMessage({ id: 'channels.dialog.setupGuide' })}</p>
            {steps.map((step, i) => (
              <p key={i} className={step.startsWith('⚠') ? 'font-semibold text-destructive' : ''}>
                {step}
              </p>
            ))}
          </div>

          <DialogField label={guide.tokenLabel}>
            <Input
              type="password"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              placeholder={intl.formatMessage({ id: 'channels.dialog.pastePlaceholder' }, { tokenLabel: guide.tokenLabel.toLowerCase() })}
            />
          </DialogField>

          {guide.secretLabel && (
            <DialogField label={guide.secretLabel}>
              <Input
                type="password"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={guide.secretLabel}
              />
            </DialogField>
          )}

          {/* G.6 — extra WhatsApp tokens (global) */}
          {channelType === 'whatsapp' && (
            <>
              <DialogField label="Verify Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={waVerifyToken} onChange={(e) => setWaVerifyToken(e.target.value)} autoComplete="off" />
              </DialogField>
              <DialogField label="App Secret" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={waAppSecret} onChange={(e) => setWaAppSecret(e.target.value)} autoComplete="off" />
              </DialogField>
            </>
          )}

          {/* G.6 — extra Feishu token (global) */}
          {channelType === 'feishu' && (
            <DialogField label="Verification Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
              <Input type="password" value={feishuVerifyToken} onChange={(e) => setFeishuVerifyToken(e.target.value)} autoComplete="off" />
            </DialogField>
          )}

          {channelType === 'teams' && (
            <DialogField label="Tenant ID" help={intl.formatMessage({ id: 'channels.setup.teams.tenantHint' })}>
              <Input type="text" value={teamsTenantId} onChange={(e) => setTeamsTenantId(e.target.value)} autoComplete="off" placeholder="(multi-tenant)" />
            </DialogField>
          )}

          {/* G.6 — extra WeCom tokens (global) */}
          {channelType === 'wecom' && (
            <>
              <DialogField label="AgentId">
                <Input type="text" value={wecomAgentId} onChange={(e) => setWecomAgentId(e.target.value)} autoComplete="off" />
              </DialogField>
              <DialogField label="Callback Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={wecomCallbackToken} onChange={(e) => setWecomCallbackToken(e.target.value)} autoComplete="off" />
              </DialogField>
              <DialogField label="EncodingAESKey" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={wecomAesKey} onChange={(e) => setWecomAesKey(e.target.value)} autoComplete="off" />
              </DialogField>
            </>
          )}

          {addError && (
            <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <AlertTriangle className="mt-0.5 size-3 shrink-0" />
              <span>{addError}</span>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'channels.dialog.cancel' })}
          </Button>
          <Button variant="brand" onClick={() => { setAddError(null); handleSubmit(); }} disabled={submitting || !token.trim()}>
            {submitting ? intl.formatMessage({ id: 'channels.dialog.adding' }) : intl.formatMessage({ id: 'channels.dialog.add' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// Pure-frontend QR renderer (no external CDN/service): encodes `value` with
/// the zero-dependency `qrcode-generator` and renders the resulting SVG.
function QrCode({ value, size = 200 }: { value: string; size?: number }) {
  const svg = useMemo(() => {
    if (!value) return '';
    // typeNumber 0 = auto-size; 'M' error correction tolerates ~15% damage.
    const qr = qrcode(0, 'M');
    qr.addData(value);
    qr.make();
    // scalable SVG so it renders crisp at any size; the input is a t.me URL we
    // control, so the generated markup is safe to inline.
    return qr.createSvgTag({ scalable: true });
  }, [value]);
  return (
    <div
      className="rounded-lg bg-white p-3"
      style={{ width: size, height: size }}
      aria-hidden
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}

/// WP9 — mint a one-time Telegram deep-link + QR that binds the company's
/// shared bot to a chosen AI employee. The employee scans the QR / opens the
/// link, sends `/start`, and every later message routes to that employee.
function TelegramBindDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const intl = useIntl();
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [selectedAgent, setSelectedAgent] = useState('');
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{
    agent: string;
    deep_link: string;
    bot_username: string;
    expires_in_minutes: number;
    max_uses: number;
  } | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (open) {
      setResult(null);
      setError(null);
      setCopied(false);
      api.agents.list().then((r) => setAgents(r.agents ?? [])).catch((e) => {
        setError(formatError(e));
      });
    }
  }, [open]);

  const handleGenerate = async () => {
    if (!selectedAgent) return;
    setGenerating(true);
    setError(null);
    try {
      const r = await api.channels.telegramBindToken(selectedAgent);
      setResult(r);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setGenerating(false);
    }
  };

  const handleCopy = async () => {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.deep_link);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      /* clipboard unavailable — the link is still visible for manual copy */
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'channels.bind.title' })}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'channels.bind.desc' })}
          </p>

          <DialogField label={intl.formatMessage({ id: 'channels.bind.selectAgent' })}>
            <Select
              value={selectedAgent}
              onValueChange={(v) => { setSelectedAgent(String(v)); setResult(null); }}
            >
              <SelectTrigger className="w-full">
                <SelectValue>
                  {selectedAgent
                    ? agents.find((a) => a.name === selectedAgent)?.display_name || selectedAgent
                    : intl.formatMessage({ id: 'channels.bind.selectPlaceholder' })}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                {agents.map((a) => (
                  <SelectItem key={a.name} value={a.name}>{a.display_name || a.name}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>

          {error && (
            <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <AlertTriangle className="mt-0.5 size-3 shrink-0" />
              <span>{error}</span>
            </div>
          )}

          {result && (
            <div className="space-y-3 rounded-lg border border-surface-border p-4">
              <div className="flex justify-center">
                <QrCode value={result.deep_link} />
              </div>
              <p className="text-center text-xs text-muted-foreground">
                {intl.formatMessage(
                  { id: 'channels.bind.hint' },
                  { bot: `@${result.bot_username}`, minutes: result.expires_in_minutes, uses: result.max_uses },
                )}
              </p>
              <div className="flex items-center gap-2">
                <code className="flex-1 overflow-x-auto whitespace-nowrap rounded-lg bg-muted px-3 py-2 font-mono text-xs">
                  {result.deep_link}
                </code>
                <Button size="sm" variant="ghost" onClick={handleCopy}>
                  {copied ? <Check /> : <Copy />}
                  {copied
                    ? intl.formatMessage({ id: 'channels.bind.copied' })
                    : intl.formatMessage({ id: 'channels.bind.copy' })}
                </Button>
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'channels.dialog.cancel' })}
          </Button>
          <Button
            variant="brand"
            onClick={handleGenerate}
            disabled={generating || !selectedAgent}
          >
            {generating
              ? intl.formatMessage({ id: 'channels.bind.generating' })
              : result
                ? intl.formatMessage({ id: 'channels.bind.regenerate' })
                : intl.formatMessage({ id: 'channels.bind.generate' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
