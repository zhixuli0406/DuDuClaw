import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type ChannelStatus, type AgentInfo } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import {
  Radio,
  Plus,
  TestTube,
  Trash2,
  CheckCircle,
  XCircle,
  Pencil,
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
  const [toast, setToast] = useState<{ type: 'success' | 'error'; message: string } | null>(null);

  const toastTimerRef = useRef<ReturnType<typeof setTimeout>>(null);
  const showToast = (type: 'success' | 'error', message: string) => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    setToast({ type, message });
    toastTimerRef.current = setTimeout(() => setToast(null), 4000);
  };
  useEffect(() => {
    return () => { if (toastTimerRef.current) clearTimeout(toastTimerRef.current); };
  }, []);

  const fetchChannels = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.channels.status();
      setChannels(result?.channels ?? []);
    } catch {
      showToast('error', '無法載入通道，請稍後再試');
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

  const handleTest = async (type: string) => {
    try {
      const result = await api.channels.test(type) as { success: boolean; message: string };
      showToast(result.success ? 'success' : 'error', result.message);
      await fetchChannels();
    } catch {
      showToast('error', '通道測試失敗，請確認設定');
    }
  };

  const handleRemove = async (type: string) => {
    if (!confirm(`確認移除 ${type} 通道？`)) return;
    try {
      await api.channels.remove(type);
      showToast('success', `${type} 通道已移除`);
      await fetchChannels();
    } catch (e) {
      showToast('error', `移除失敗: ${e}`);
    }
  };

  return (
    <div className="space-y-6">
      {/* Toast notification */}
      {toast && (
        <div className={cn(
          'rounded-lg px-4 py-3 text-sm',
          toast.type === 'success'
            ? 'bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400'
            : 'bg-rose-50 text-rose-700 dark:bg-rose-900/20 dark:text-rose-400'
        )}>
          {toast.message}
        </div>
      )}

      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'channels.title' })}
        </h2>
        <button
          onClick={() => setShowAddDialog(true)}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600"
        >
          <Plus className="h-4 w-4" />
          {intl.formatMessage({ id: 'channels.add' })}
        </button>
      </div>

      {channels.length === 0 && !loading ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Radio className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'channels.empty' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {channels.map((channel) => {
            const style = getChannelStyle(channel.name);
            return (
              <div
                key={channel.name}
                className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900"
              >
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <div
                      className={cn(
                        'rounded-lg p-2.5',
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
                        <p className="text-xs text-stone-500 dark:text-stone-400">
                          {new Date(channel.last_connected).toLocaleString(
                            'zh-TW'
                          )}
                        </p>
                      )}
                    </div>
                  </div>

                  {/* Connection status */}
                  {channel.connected ? (
                    <span className="inline-flex items-center gap-1 rounded-full bg-emerald-100 px-2.5 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400">
                      <CheckCircle className="h-3 w-3" />
                      {intl.formatMessage({ id: 'status.connected' })}
                    </span>
                  ) : (
                    <span className="inline-flex items-center gap-1 rounded-full bg-rose-100 px-2.5 py-0.5 text-xs font-medium text-rose-700 dark:bg-rose-900/30 dark:text-rose-400">
                      <XCircle className="h-3 w-3" />
                      {intl.formatMessage({ id: 'status.disconnected' })}
                    </span>
                  )}
                </div>

                {/* Error message */}
                {channel.error && (
                  <div className="mt-3 rounded-lg bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
                    {channel.error}
                  </div>
                )}

                {/* Actions */}
                <div className="mt-4 flex gap-2 border-t border-stone-100 pt-3 dark:border-stone-800">
                  <button
                    onClick={() => handleTest(channel.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                  >
                    <TestTube className="h-3.5 w-3.5" />
                    {intl.formatMessage({ id: 'channels.test' })}
                  </button>
                  <button
                    onClick={() => setEditChannel(channel.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                  >
                    <Pencil className="h-3.5 w-3.5" />
                    編輯
                  </button>
                  <button
                    onClick={() => handleRemove(channel.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-900/20"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                    {intl.formatMessage({ id: 'channels.remove' })}
                  </button>
                </div>
              </div>
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
    </div>
  );
}

const SUPPORTS_PER_AGENT = ['discord', 'telegram', 'slack'];

function AddChannelDialog({ open, onClose, onCreated, fixedType }: { open: boolean; onClose: () => void; onCreated: () => void; fixedType?: string }) {
  const [channelType, setChannelType] = useState(fixedType ?? 'line');
  const [selectedAgent, setSelectedAgent] = useState('');
  const [agents, setAgents] = useState<AgentInfo[]>([]);

  useEffect(() => {
    if (fixedType) setChannelType(fixedType);
  }, [fixedType]);

  useEffect(() => {
    if (open) {
      api.agents.list().then((r) => setAgents(r.agents ?? [])).catch(() => {});
    }
  }, [open]);

  const [token, setToken] = useState('');
  const [secret, setSecret] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async () => {
    if (!token.trim()) return;
    setSubmitting(true);
    try {
      const config: Record<string, string> = { token: token.trim() };
      if (secret.trim()) config.secret = secret.trim();
      await api.channels.add(channelType, config, selectedAgent || undefined);
      onCreated();
      onClose();
      setToken('');
      setSecret('');
      setSelectedAgent('');
    } catch {
      // error
    } finally {
      setSubmitting(false);
    }
  };

  const channelGuide: Record<string, { tokenLabel: string; secretLabel?: string; steps: string[] }> = {
    telegram: {
      tokenLabel: 'Bot Token',
      steps: [
        '1. 在 Telegram 搜尋 @BotFather 並開始對話',
        '2. 輸入 /newbot，依提示設定名稱與 username',
        '3. BotFather 會回傳 Bot Token，複製貼到下方',
        'Long Polling 模式，無需設定 Webhook',
      ],
    },
    line: {
      tokenLabel: 'Channel Access Token',
      secretLabel: 'Channel Secret',
      steps: [
        '1. 前往 developers.line.biz/console',
        '2. 建立 Provider → Messaging API Channel',
        '3. Basic settings → 複製 Channel Secret',
        '4. Messaging API → Issue Channel Access Token',
        '5. Webhook settings → 設定 URL + 開啟 Use webhook',
        '需要 HTTPS（ngrok / Tailscale Funnel）',
      ],
    },
    discord: {
      tokenLabel: 'Bot Token',
      steps: [
        '1. 前往 discord.com/developers/applications',
        '2. New Application → 左側 Bot → Reset Token → 複製 Token',
        '3. Bot 頁面往下捲 → Privileged Gateway Intents：',
        '⚠️ 必須開啟 MESSAGE CONTENT INTENT（否則 Bot 無法收到訊息）',
        '   建議開啟 SERVER MEMBERS INTENT',
        '4. 左側 OAuth2 → URL Generator → Scopes 勾選 bot',
        '5. Bot Permissions 勾選：',
        '   ☑ Send Messages（傳送訊息）',
        '   ☑ Read Message History（讀取訊息歷史記錄）',
        '   ☑ View Channels（檢視頻道）',
        '6. 複製產生的 URL → 瀏覽器開啟 → 邀請 Bot 加入伺服器',
        '💡 若先前已邀請但權限不足，需用新 URL 重新邀請',
      ],
    },
    slack: {
      tokenLabel: 'Bot User OAuth Token (xoxb-...)',
      secretLabel: 'App-Level Token (xapp-...)',
      steps: [
        '1. 前往 api.slack.com/apps → Create New App',
        '2. OAuth & Permissions → Install to Workspace',
        '3. 複製 Bot User OAuth Token (xoxb-...)',
        '4. Socket Mode → 啟用 → 取得 App-Level Token (xapp-...)',
        '5. OAuth Scopes: chat:write, channels:read, app_mentions:read',
        'Socket Mode 模式，無需公開 URL',
      ],
    },
    whatsapp: {
      tokenLabel: 'Access Token',
      secretLabel: 'Phone Number ID',
      steps: [
        '1. 前往 developers.facebook.com/apps',
        '2. 建立 Business App → 加入 WhatsApp 產品',
        '3. WhatsApp → API Setup → 取得 Access Token',
        '4. 記下 Phone Number ID',
        '5. Configuration → 設定 Webhook URL + Verify Token',
        '6. 訂閱 messages 事件',
        '需要 Meta Business 驗證才能正式上線',
      ],
    },
    feishu: {
      tokenLabel: 'App ID',
      secretLabel: 'App Secret',
      steps: [
        '1. 前往 open.feishu.cn/app',
        '2. 建立企業自建應用',
        '3. 憑證與基礎資訊 → 取得 App ID + App Secret',
        '4. 事件與回調 → 設定 Request URL',
        '5. 權限: im:message:send_as_bot, im:message',
        '6. 提交審核 → 發布上線',
      ],
    },
  };

  const guide = channelGuide[channelType] ?? { tokenLabel: 'Token', steps: [] };

  return (
    <Dialog open={open} onClose={onClose} title={fixedType ? `Edit ${fixedType}` : 'Add Channel'}>
      <div className="space-y-4">
        <FormField label="Channel Type">
          <select value={channelType} onChange={(e) => setChannelType(e.target.value)} disabled={!!fixedType} className={selectClass}>
            <option value="telegram">Telegram</option>
            <option value="line">LINE</option>
            <option value="discord">Discord</option>
            <option value="slack">Slack</option>
            <option value="whatsapp">WhatsApp</option>
            <option value="feishu">Feishu</option>
          </select>
        </FormField>

        {SUPPORTS_PER_AGENT.includes(channelType) && agents.length > 0 && (
          <FormField label="指定 Agent（選填）">
            <select value={selectedAgent} onChange={(e) => setSelectedAgent(e.target.value)} className={selectClass}>
              <option value="">全域（Global）</option>
              {agents.map((a) => (
                <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
              ))}
            </select>
            <p className="mt-1 text-xs text-stone-500 dark:text-stone-400">
              指定後此 Bot 僅服務該 Agent，不指定則為全域通道
            </p>
          </FormField>
        )}

        {/* Setup guide */}
        <div className="rounded-lg bg-amber-50 p-3 text-xs text-amber-800 dark:bg-amber-900/20 dark:text-amber-300">
          <p className="mb-1 font-medium">Setup Guide:</p>
          {guide.steps.map((step, i) => (
            <p key={i} className={step.startsWith('⚠') ? 'font-semibold text-rose-600 dark:text-rose-400' : ''}>
              {step}
            </p>
          ))}
        </div>

        <FormField label={guide.tokenLabel}>
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={`Paste your ${guide.tokenLabel.toLowerCase()}...`}
            className={inputClass}
          />
        </FormField>

        {guide.secretLabel && (
          <FormField label={guide.secretLabel}>
            <input
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={guide.secretLabel}
              className={inputClass}
            />
          </FormField>
        )}

        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>Cancel</button>
          <button onClick={handleSubmit} disabled={submitting || !token.trim()} className={buttonPrimary}>
            {submitting ? 'Adding...' : 'Add Channel'}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
