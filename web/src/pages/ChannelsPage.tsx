import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type ChannelStatus } from '@/lib/api';
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
};

function getChannelStyle(name: string) {
  const key = name.toLowerCase();
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

function AddChannelDialog({ open, onClose, onCreated, fixedType }: { open: boolean; onClose: () => void; onCreated: () => void; fixedType?: string }) {
  const [channelType, setChannelType] = useState(fixedType ?? 'line');

  useEffect(() => {
    if (fixedType) setChannelType(fixedType);
  }, [fixedType]);
  const [token, setToken] = useState('');
  const [secret, setSecret] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async () => {
    if (!token.trim()) return;
    setSubmitting(true);
    try {
      const config: Record<string, string> = { token: token.trim() };
      if (secret.trim()) config.secret = secret.trim();
      await api.channels.add(channelType, config);
      onCreated();
      onClose();
      setToken('');
      setSecret('');
    } catch {
      // error
    } finally {
      setSubmitting(false);
    }
  };

  const tokenLabel: Record<string, string> = {
    line: 'Channel Access Token',
    telegram: 'Bot Token',
    discord: 'Bot Token',
  };

  return (
    <Dialog open={open} onClose={onClose} title={fixedType ? `編輯 ${fixedType} 通道` : '新增通道'}>
      <div className="space-y-4">
        <FormField label="通道類型">
          <select value={channelType} onChange={(e) => setChannelType(e.target.value)} disabled={!!fixedType} className={selectClass}>
            <option value="line">LINE</option>
            <option value="telegram">Telegram</option>
            <option value="discord">Discord</option>
          </select>
        </FormField>

        <FormField label={tokenLabel[channelType] ?? 'Token'}>
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder="貼上你的 token..."
            className={inputClass}
          />
        </FormField>

        {channelType === 'line' && (
          <FormField label="Channel Secret">
            <input
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder="LINE Channel Secret"
              className={inputClass}
            />
          </FormField>
        )}

        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>取消</button>
          <button onClick={handleSubmit} disabled={submitting || !token.trim()} className={buttonPrimary}>
            {submitting ? '新增中...' : '新增通道'}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
