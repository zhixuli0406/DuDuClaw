import { useEffect, useState, useCallback, useRef, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import qrcode from 'qrcode-generator';
import { cn } from '@/lib/utils';
import {
  api,
  type ChannelStatus,
  type AgentInfo,
  type ChannelConfigSettings,
  type ChannelAccessSettings,
} from '@/lib/api';
import { client } from '@/lib/ws-client';
import { toast, formatError } from '@/lib/toast';
import { useConnectionStore } from '@/stores/connection-store';
import { ConfirmDialog } from '@/components/settings/controls';
import { AddChannelDialog, CHANNEL_TYPES, DialogField } from '@/components/channels/AddChannelDialog';
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
  ErrorState,
  Badge,
  Switch,
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
  Spinner,
} from '@/components/mds';
import {
  Radio,
  Plus,
  TestTube,
  Trash2,
  CheckCircle,
  Pencil,
  AlertTriangle,
  Info,
  X,
  Link2,
  Copy,
  Check,
  MoreHorizontal,
  SlidersHorizontal,
  ShieldCheck,
  QrCode as QrCodeIcon,
  Printer,
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
  /** Set when `channels.status` threw — keeps a failed load out of the empty state. */
  const [loadError, setLoadError] = useState<unknown>(null);
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [showBindDialog, setShowBindDialog] = useState(false);
  const [showLineQr, setShowLineQr] = useState(false);

  // WP1.2 First-Win deep link: /manage/channels?lineQr=1 opens the LINE QR
  // dialog directly — used by onboarding hand-offs and docs.
  useEffect(() => {
    if (new URLSearchParams(window.location.search).get('lineQr') === '1') {
      setShowLineQr(true);
    }
  }, []);
  const [editChannel, setEditChannel] = useState<string | null>(null);
  const [removeTarget, setRemoveTarget] = useState<string | null>(null);
  // W2-2 (E1/E2) — 行為 + 存取 detail dialog, keyed by channel row name
  // (e.g. "discord" or "discord:sam" — the dialog resolves it to the base
  // platform, since behavior/access settings are per-platform, not per-agent).
  const [detailChannel, setDetailChannel] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  // 'warning' covers the honest "credential_only" degrade — a test that
  // could not actually send a message must never read as a green success.
  const [toast, setToast] = useState<{ type: 'success' | 'warning' | 'error'; message: string } | null>(null);

  const toastTimerRef = useRef<ReturnType<typeof setTimeout>>(null);
  const showToast = useCallback((type: 'success' | 'warning' | 'error', message: string) => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    setToast({ type, message });
    toastTimerRef.current = setTimeout(() => setToast(null), type === 'success' ? 4000 : 8000);
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
    setLoadError(null);
    try {
      const result = await api.channels.status();
      setChannels(result?.channels ?? []);
    } catch (e) {
      // P05: `channels` stays [] on failure, which renders exactly like "no
      // channels configured" — the reading that sends a user off to add a bot
      // they already have. Keep the failure on screen, with a retry.
      setLoadError(e);
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
      const result = await api.channels.test(type);
      // `sent` is the only signal that a message actually left the server —
      // `mode: "credential_only"` means only the token was checked, so it
      // must never render as a success (that was breakpoint #5: a revoked
      // token used to show a green "測試成功").
      const toastType = result.sent ? 'success' : result.mode === 'credential_only' ? 'warning' : 'error';
      showToast(toastType, result.detail);
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
          <Button variant="outline" size="sm" onClick={() => setShowLineQr(true)}>
            <QrCodeIcon />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'channels.lineQr.action' })}</span>
          </Button>
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
            : toast.type === 'warning'
              ? 'bg-warning/10 text-warning'
              : 'bg-destructive/10 text-destructive'
        )}>
          {toast.type === 'success' ? (
            <CheckCircle className="mt-0.5 size-4 shrink-0" />
          ) : toast.type === 'warning' ? (
            <Info className="mt-0.5 size-4 shrink-0" />
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

      {loadError != null && channels.length === 0 && !loading ? (
        <ErrorState
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
          onRetry={() => void fetchChannels()}
        />
      ) : channels.length === 0 && !loading ? (
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
              onDetail={() => setDetailChannel(channel.name)}
            />
          ))}
        </div>
      )}

      {/* Add Channel Dialog */}
      <AddChannelDialog
        open={showAddDialog}
        onClose={() => setShowAddDialog(false)}
        onCreated={(createdType) => {
          fetchChannels();
          if (createdType === 'line') setShowLineQr(true);
        }}
      />

      {/* WP9 — Telegram shared-bot employee bind link / QR */}
      <TelegramBindDialog
        open={showBindDialog}
        onClose={() => setShowBindDialog(false)}
      />

      {/* WP1.1 (ecosystem) — LINE OA add-friend QR + printable poster */}
      <LineQrDialog open={showLineQr} onClose={() => setShowLineQr(false)} />

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

      {/* W2-2 (E1/E2) — 行為 / 存取 detail dialog */}
      <ChannelDetailDialog
        channelName={detailChannel}
        open={detailChannel !== null}
        onClose={() => setDetailChannel(null)}
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
  onDetail,
}: {
  channel: ChannelStatus;
  onTest: () => void;
  onEdit: () => void;
  onRemove: () => void;
  onDetail: () => void;
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
              <DropdownMenuItem onClick={onDetail}>
                <SlidersHorizontal />
                {intl.formatMessage({ id: 'channels.detail.action' })}
              </DropdownMenuItem>
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

/// Minimal HTML-escape for text interpolated into the poster popup.
function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c] as string
  );
}

/// WP1.1 (ecosystem) — LINE OA add-friend QR. Fetches the OA's deep link,
/// renders a local QR (no external QR service), and prints an A-size poster
/// via a minimal popup + window.print() — zero PDF dependencies.
function LineQrDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const intl = useIntl();
  const [info, setInfo] = useState<{
    add_friend_url: string;
    basic_id: string;
    display_name: string | null;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!open) return;
    setInfo(null);
    setError(null);
    setCopied(false);
    setLoading(true);
    api.channels
      .lineAddFriend()
      .then(setInfo)
      .catch((e) => setError(formatError(e)))
      .finally(() => setLoading(false));
  }, [open]);

  const copyLink = () => {
    if (!info) return;
    navigator.clipboard.writeText(info.add_friend_url).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  const printPoster = () => {
    if (!info) return;
    const qr = qrcode(0, 'M');
    qr.addData(info.add_friend_url);
    qr.make();
    const svg = qr.createSvgTag({ scalable: true });
    const title = escapeHtml(info.display_name ?? 'DuDuClaw AI');
    const scanLine = escapeHtml(intl.formatMessage({ id: 'channels.lineQr.posterScan' }));
    const idLine = escapeHtml(info.basic_id);
    const w = window.open('', '_blank', 'width=520,height=720');
    if (!w) return;
    w.document.write(`<!DOCTYPE html><html><head><meta charset="utf-8"><title>${title}</title>
<style>
  body { font-family: system-ui, -apple-system, "PingFang TC", sans-serif; text-align: center;
         margin: 0; padding: 40px 24px; color: #1c1917; }
  h1 { font-size: 26px; margin: 0 0 6px; }
  .id { color: #57534e; font-size: 15px; margin-bottom: 22px; }
  .qr { width: 300px; height: 300px; margin: 0 auto 22px; }
  .qr svg { width: 100%; height: 100%; }
  .scan { font-size: 19px; font-weight: 600; }
  .brand { margin-top: 28px; font-size: 12px; color: #a8a29e; }
  @media print { .brand { color: #a8a29e; } }
</style></head><body>
<h1>${title}</h1><div class="id">LINE ID：${idLine}</div>
<div class="qr">${svg}</div><div class="scan">${scanLine}</div>
<div class="brand">Powered by DuDuClaw 🐾</div>
</body></html>`);
    w.document.close();
    w.focus();
    w.print();
  };

  return (
    <Dialog open={open} onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'channels.lineQr.title' })}</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'channels.lineQr.desc' })}
        </p>
        {loading && (
          <p className="py-6 text-center text-sm text-muted-foreground">…</p>
        )}
        {error && <p className="py-4 text-sm text-destructive">{error}</p>}
        {info && (
          <div className="flex flex-col items-center gap-3 py-2">
            <QrCode value={info.add_friend_url} size={220} />
            <div className="text-center">
              <p className="font-medium">{info.display_name ?? 'LINE OA'}</p>
              <p className="text-sm text-muted-foreground">{info.basic_id}</p>
            </div>
            <div className="flex gap-2">
              <Button variant="outline" size="sm" onClick={copyLink}>
                {copied ? <Check /> : <Copy />}
                {intl.formatMessage({
                  id: copied ? 'channels.lineQr.copied' : 'channels.lineQr.copyLink',
                })}
              </Button>
              <Button variant="brand" size="sm" onClick={printPoster}>
                <Printer />
                {intl.formatMessage({ id: 'channels.lineQr.print' })}
              </Button>
            </div>
            {/* WP1.7 — NFC touchpoint: the same deep link doubles as the NFC
                tag payload. Write it to an NTAG213 with any NFC-writer app
                and the table card becomes "tap to chat". */}
            <div className="mt-2 w-full rounded-lg border border-border p-3 text-sm">
              <p className="font-medium">
                {intl.formatMessage({ id: 'channels.lineQr.nfcTitle' })}
              </p>
              <p className="mt-1 text-muted-foreground">
                {intl.formatMessage({ id: 'channels.lineQr.nfcDesc' })}
              </p>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
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

// ── W2-2 (E1/E2) — 行為 / 存取 detail dialog ─────────────────────────────
//
// `channels.config_get/set` (行為) and `channels.access_get/set` +
// `channels.pairing_list/revoke` (存取) close the biggest gap between what
// ChannelsPage shows and what a channel's admin/agent could already change
// via chat commands + MCP tools (`channel_config`/`pairing_manage`) — see
// `commercial/docs/ux-redesign-2026-08/01-current-state-map.md` breakpoint #3.
// Both tabs write through the same `ChannelSettingsManager`/`AccessController`
// the MCP tools use, so a change made from a chat and a change made here
// converge on one state (`dashboard_feedback.rs` pushes `channel_config.changed`
// either way — no manual refresh needed to see the other side's edit).

/** Discord-only behavior keys — currently only `discord.rs` reads them
 * (guild allowlist / auto-thread / response embed style / archive timer).
 * Shown conditionally so a Telegram/LINE admin never sees dead controls. */
const DISCORD_ONLY_BEHAVIOR = true;

const THREAD_ARCHIVE_MINUTES: ReadonlyArray<string> = ['60', '1440', '4320', '10080'];

function channelDisplayLabel(platform: string): string {
  return CHANNEL_TYPES.find((c) => c.value === platform)?.label ?? platform;
}

function ChannelDetailDialog({
  channelName,
  open,
  onClose,
}: {
  channelName: string | null;
  open: boolean;
  onClose: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  const t = (id: string) => intl.formatMessage({ id });
  const platform = channelName ? getChannelPlatform(channelName) : '';
  const [tab, setTab] = useState<'behavior' | 'access'>('behavior');

  useEffect(() => {
    if (open) setTab('behavior');
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {intl.formatMessage({ id: 'channels.detail.title' }, { name: channelDisplayLabel(platform) })}
          </DialogTitle>
        </DialogHeader>

        {/* R-cross-link (§2-1): "路由訊息給哪位 AI 員工" (this dialog's Behavior
            tab) is a different setting from "授權存取" (who can operate that AI
            員工) and "驗證身分" (who is who on the channel) — both live on
            UsersPage. Point admins there instead of letting them assume this
            dialog covers everything. */}
        <div className="flex items-start gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
          <Info className="mt-0.5 size-3.5 shrink-0" />
          <span className="flex-1">{t('channels.detail.crossLink')}</span>
          <Button
            variant="link"
            size="sm"
            className="h-auto shrink-0 p-0"
            onClick={() => navigate('/manage/users')}
          >
            {t('channels.detail.crossLink.action')}
          </Button>
        </div>

        {platform && (
          <Tabs value={tab} onValueChange={(v) => setTab(v as 'behavior' | 'access')}>
            <TabsList>
              <TabsTab value="behavior">
                <SlidersHorizontal />
                {t('channels.detail.tab.behavior')}
              </TabsTab>
              <TabsTab value="access">
                <ShieldCheck />
                {t('channels.detail.tab.access')}
              </TabsTab>
            </TabsList>
            <div className="max-h-[60vh] overflow-y-auto pt-4">
              <TabsPanel value="behavior">
                <ChannelBehaviorTab platform={platform} active={open && tab === 'behavior'} />
              </TabsPanel>
              <TabsPanel value="access">
                <ChannelAccessTab platform={platform} active={open && tab === 'access'} />
              </TabsPanel>
            </div>
          </Tabs>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.close')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** Add/remove chip list editor for `allowed_*`/`blocked_*`/`admin_users`
 * fields. Enter or the + button adds the current draft; duplicates are
 * silently ignored rather than erroring (the same value re-added is a no-op,
 * not a mistake worth interrupting the admin over). */
function TagListEditor({
  label,
  help,
  value,
  onChange,
  placeholder,
}: {
  label: string;
  help?: string;
  value: string[];
  onChange: (next: string[]) => void;
  placeholder?: string;
}) {
  const intl = useIntl();
  const [draft, setDraft] = useState('');

  const add = () => {
    const v = draft.trim();
    if (!v) return;
    if (!value.includes(v)) onChange([...value, v]);
    setDraft('');
  };

  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground">{label}</label>
      <div className="flex gap-2">
        <Input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder={placeholder}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              add();
            }
          }}
        />
        <Button type="button" variant="outline" size="sm" onClick={add} disabled={!draft.trim()}>
          <Plus />
          {intl.formatMessage({ id: 'common.add' })}
        </Button>
      </div>
      {value.length > 0 && (
        <div className="flex flex-wrap gap-1.5 pt-0.5">
          {value.map((v) => (
            <Badge key={v} variant="secondary" className="gap-1 pr-1">
              <span className="font-mono">{v}</span>
              <button
                type="button"
                className="rounded p-0.5 opacity-60 transition-opacity hover:opacity-100"
                onClick={() => onChange(value.filter((x) => x !== v))}
                aria-label={intl.formatMessage({ id: 'common.remove' })}
              >
                <X className="size-3" />
              </button>
            </Badge>
          ))}
        </div>
      )}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

/** 行為 tab (E1) — `channels.config_get`/`channels.config_set`. */
function ChannelBehaviorTab({ platform, active }: { platform: string; active: boolean }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settings, setSettings] = useState<ChannelConfigSettings | null>(null);
  const [saveState, setSaveState] = useState<'idle' | 'saving' | 'saved' | 'error'>('idle');

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    api.channels.configGet(platform)
      .then((r) => { if (!cancelled) setSettings(r.settings); })
      .catch((e) => { if (!cancelled) setLoadError(formatError(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [active, platform]);

  const update = <K extends keyof ChannelConfigSettings>(key: K, value: ChannelConfigSettings[K]) => {
    setSettings((prev) => (prev ? { ...prev, [key]: value } : prev));
  };

  const handleSave = async () => {
    if (!settings) return;
    setSaveState('saving');
    try {
      await api.channels.configSet(platform, settings);
      setSaveState('saved');
      toast.success(t('channels.detail.saved'));
      setTimeout(() => setSaveState('idle'), 2000);
    } catch (e) {
      setSaveState('error');
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Spinner className="size-5 text-muted-foreground" />
      </div>
    );
  }
  if (loadError || !settings) {
    return (
      <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
        <AlertTriangle className="mt-0.5 size-3 shrink-0" />
        <span>{loadError ?? t('channels.detail.loadFailed')}</span>
      </div>
    );
  }

  const isDiscord = DISCORD_ONLY_BEHAVIOR && platform === 'discord';

  return (
    <div className="space-y-5">
      <SettingsSection>
        <SettingsCard>
          <SettingsRow
            label={t('channels.detail.behavior.mentionOnly')}
            description={t('channels.detail.behavior.mentionOnly.help')}
          >
            <Switch
              checked={settings.mention_only}
              onCheckedChange={(v) => update('mention_only', Boolean(v))}
            />
          </SettingsRow>
          <SettingsRow
            label={t('channels.detail.behavior.agentOverride')}
            description={t('channels.detail.behavior.agentOverride.help')}
            tier="text"
          >
            <Input
              value={settings.agent_override}
              onChange={(e) => update('agent_override', e.target.value)}
              placeholder={t('channels.detail.behavior.agentOverride.placeholder')}
            />
          </SettingsRow>
          {isDiscord && (
            <SettingsRow
              label={t('channels.detail.behavior.autoThread')}
              description={t('channels.detail.behavior.autoThread.help')}
            >
              <Switch
                checked={settings.auto_thread}
                onCheckedChange={(v) => update('auto_thread', Boolean(v))}
              />
            </SettingsRow>
          )}
          {isDiscord && (
            <SettingsRow label={t('channels.detail.behavior.responseMode')} tier="select">
              <Select
                value={settings.response_mode}
                onValueChange={(v) => update('response_mode', String(v) as ChannelConfigSettings['response_mode'])}
              >
                <SelectTrigger className="w-full">
                  <SelectValue>{t(`channels.detail.behavior.responseMode.${settings.response_mode}`)}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="auto">{t('channels.detail.behavior.responseMode.auto')}</SelectItem>
                  <SelectItem value="embed">{t('channels.detail.behavior.responseMode.embed')}</SelectItem>
                  <SelectItem value="plain">{t('channels.detail.behavior.responseMode.plain')}</SelectItem>
                </SelectContent>
              </Select>
            </SettingsRow>
          )}
          {isDiscord && (
            <SettingsRow label={t('channels.detail.behavior.threadArchive')} tier="select">
              <Select
                value={settings.thread_archive_minutes ?? '__unset'}
                onValueChange={(v) => update('thread_archive_minutes', String(v) === '__unset' ? null : String(v))}
              >
                <SelectTrigger className="w-full">
                  <SelectValue>
                    {settings.thread_archive_minutes
                      ? t(`channels.detail.behavior.threadArchive.${settings.thread_archive_minutes}`)
                      : t('channels.detail.behavior.threadArchive.unset')}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__unset">{t('channels.detail.behavior.threadArchive.unset')}</SelectItem>
                  {THREAD_ARCHIVE_MINUTES.map((minutes) => (
                    <SelectItem key={minutes} value={minutes}>
                      {t(`channels.detail.behavior.threadArchive.${minutes}`)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </SettingsRow>
          )}
        </SettingsCard>
      </SettingsSection>

      <TagListEditor
        label={t('channels.detail.behavior.allowedChannels')}
        help={t('channels.detail.behavior.allowedChannels.help')}
        value={settings.allowed_channels}
        onChange={(v) => update('allowed_channels', v)}
        placeholder={t('channels.detail.behavior.allowedChannels.placeholder')}
      />
      {isDiscord && (
        <TagListEditor
          label={t('channels.detail.behavior.allowedGuilds')}
          help={t('channels.detail.behavior.allowedGuilds.help')}
          value={settings.allowed_guilds}
          onChange={(v) => update('allowed_guilds', v)}
          placeholder={t('channels.detail.behavior.allowedGuilds.placeholder')}
        />
      )}

      <div className="flex items-center justify-end gap-3 pt-1">
        <SettingsSaveState
          status={saveState}
          savingLabel={t('common.saving')}
          savedLabel={t('channels.detail.saved.inline')}
          errorLabel={t('common.saveError')}
        />
        <Button variant="brand" size="sm" onClick={handleSave} disabled={saveState === 'saving'}>
          {t('common.save')}
        </Button>
      </div>
    </div>
  );
}

/** 存取 tab (E2) — `channels.access_get`/`channels.access_set` +
 * `channels.pairing_list`/`channels.pairing_revoke`. */
function ChannelAccessTab({ platform, active }: { platform: string; active: boolean }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settings, setSettings] = useState<ChannelAccessSettings | null>(null);
  const [saveState, setSaveState] = useState<'idle' | 'saving' | 'saved' | 'error'>('idle');

  const [approved, setApproved] = useState<string[]>([]);
  const [pairingLoading, setPairingLoading] = useState(true);
  const [revoking, setRevoking] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    api.channels.accessGet(platform)
      .then((r) => { if (!cancelled) setSettings(r.settings); })
      .catch((e) => { if (!cancelled) setLoadError(formatError(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [active, platform]);

  const loadPairing = useCallback(() => {
    setPairingLoading(true);
    api.channels.pairingList()
      .then((r) => setApproved(r.approved ?? []))
      .catch((e) => toast.error(formatError(e)))
      .finally(() => setPairingLoading(false));
  }, []);

  useEffect(() => {
    if (active) loadPairing();
  }, [active, loadPairing]);

  // Live-refresh when the same store changes from the other write path (a
  // channel `/pair` approval, or another dashboard tab) — closes the E1/E2
  // sync gap called out in the task brief.
  useEffect(() => {
    const unsubscribe = client.subscribe('channel_config.changed', () => {
      if (active) loadPairing();
    });
    return unsubscribe;
  }, [active, loadPairing]);

  const update = <K extends keyof ChannelAccessSettings>(key: K, value: ChannelAccessSettings[K]) => {
    setSettings((prev) => (prev ? { ...prev, [key]: value } : prev));
  };

  const handleSave = async () => {
    if (!settings) return;
    setSaveState('saving');
    try {
      await api.channels.accessSet(platform, settings);
      setSaveState('saved');
      toast.success(t('channels.detail.saved'));
      setTimeout(() => setSaveState('idle'), 2000);
    } catch (e) {
      setSaveState('error');
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    }
  };

  const handleRevoke = async (subject: string) => {
    setRevoking(subject);
    try {
      await api.channels.pairingRevoke(subject);
      setApproved((prev) => prev.filter((s) => s !== subject));
      toast.success(intl.formatMessage({ id: 'channels.detail.access.pairing.revoked' }, { subject }));
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setRevoking(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Spinner className="size-5 text-muted-foreground" />
      </div>
    );
  }
  if (loadError || !settings) {
    return (
      <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
        <AlertTriangle className="mt-0.5 size-3 shrink-0" />
        <span>{loadError ?? t('channels.detail.loadFailed')}</span>
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <SettingsSection>
        <SettingsCard>
          <SettingsRow
            label={t('channels.detail.access.requirePairing')}
            description={t('channels.detail.access.requirePairing.help')}
          >
            <Switch
              checked={settings.require_pairing}
              onCheckedChange={(v) => update('require_pairing', Boolean(v))}
            />
          </SettingsRow>
        </SettingsCard>
      </SettingsSection>

      <TagListEditor
        label={t('channels.detail.access.allowedUsers')}
        help={t('channels.detail.access.allowedUsers.help')}
        value={settings.allowed_users}
        onChange={(v) => update('allowed_users', v)}
        placeholder={t('channels.detail.access.allowedUsers.placeholder')}
      />
      <TagListEditor
        label={t('channels.detail.access.blockedUsers')}
        help={t('channels.detail.access.blockedUsers.help')}
        value={settings.blocked_users}
        onChange={(v) => update('blocked_users', v)}
        placeholder={t('channels.detail.access.blockedUsers.placeholder')}
      />
      <TagListEditor
        label={t('channels.detail.access.adminUsers')}
        help={t('channels.detail.access.adminUsers.help')}
        value={settings.admin_users}
        onChange={(v) => update('admin_users', v)}
        placeholder={t('channels.detail.access.adminUsers.placeholder')}
      />

      <div className="flex items-center justify-end gap-3 pt-1">
        <SettingsSaveState
          status={saveState}
          savingLabel={t('common.saving')}
          savedLabel={t('channels.detail.saved.inline')}
          errorLabel={t('common.saveError')}
        />
        <Button variant="brand" size="sm" onClick={handleSave} disabled={saveState === 'saving'}>
          {t('common.save')}
        </Button>
      </div>

      {/* Approved pairing subjects — shared across every channel type, so
          this list looks the same regardless of which platform tab it's
          opened from (see channels.pairing_list — no per-channel dimension
          exists server-side). */}
      <SettingsSection
        title={t('channels.detail.access.pairing.title')}
        description={t('channels.detail.access.pairing.desc')}
      >
        <SettingsCard>
          {pairingLoading ? (
            <div className="flex items-center justify-center py-8">
              <Spinner className="size-4 text-muted-foreground" />
            </div>
          ) : approved.length === 0 ? (
            <p className="px-4 py-4 text-center text-xs text-muted-foreground">
              {t('channels.detail.access.pairing.empty')}
            </p>
          ) : (
            approved.map((subject) => (
              <div key={subject} className="flex items-center justify-between gap-3 px-4 py-2.5">
                <span className="truncate font-mono text-xs text-foreground">{subject}</span>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={revoking === subject}
                  onClick={() => handleRevoke(subject)}
                >
                  <X className="size-3.5" />
                  {t('channels.detail.access.pairing.revoke')}
                </Button>
              </div>
            ))
          )}
        </SettingsCard>
      </SettingsSection>
    </div>
  );
}
