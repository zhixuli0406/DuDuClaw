import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  MCP_SCOPES,
  type McpKeyEntry,
  type McpKeyCreateResult,
  type McpScope,
} from '@/lib/api';
import { Dialog, FormField, inputClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import { KeyRound, Plus, Trash2, Copy, Check, AlertTriangle } from 'lucide-react';

export function McpKeysPage() {
  const intl = useIntl();
  const [keys, setKeys] = useState<ReadonlyArray<McpKeyEntry>>([]);
  const [loading, setLoading] = useState(false);
  const [showCreate, setShowCreate] = useState(false);
  const [created, setCreated] = useState<McpKeyCreateResult | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);

  const fetchKeys = useCallback(async () => {
    setLoading(true);
    try {
      const res = await api.mcpKeys.list();
      setKeys(res?.keys ?? []);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    fetchKeys();
  }, [fetchKeys]);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'mcpKeys.title' })}
        </h2>
        <button
          onClick={() => setShowCreate(true)}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600"
        >
          <Plus className="h-4 w-4" />
          {intl.formatMessage({ id: 'mcpKeys.create' })}
        </button>
      </div>

      <p className="text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'mcpKeys.desc' })}
      </p>

      <div className="glass-card rounded-2xl p-6">
        {loading ? (
          <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
        ) : keys.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-12">
            <KeyRound className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
            <p className="text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.empty' })}</p>
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-stone-200 dark:border-stone-700">
                  <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.key' })}</th>
                  <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.clientId' })}</th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.external' })}</th>
                  <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.scopes' })}</th>
                  <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.created' })}</th>
                  <th className="py-2 text-right font-medium text-stone-500 dark:text-stone-400" />
                </tr>
              </thead>
              <tbody>
                {keys.map((k) => (
                  <tr key={k.masked} className="border-b border-stone-100 dark:border-stone-800">
                    <td className="py-2.5">
                      <code className="rounded bg-stone-100 px-2 py-0.5 font-mono text-xs text-stone-700 dark:bg-stone-800 dark:text-stone-300">
                        {k.masked}
                      </code>
                      {k.rotate_recommended && (
                        <span className="ml-2 inline-flex items-center gap-1 rounded-full bg-amber-100 px-2 py-0.5 text-[11px] font-medium text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">
                          <AlertTriangle className="h-3 w-3" />
                          {intl.formatMessage({ id: 'mcpKeys.rotate' })}
                        </span>
                      )}
                    </td>
                    <td className="py-2.5 text-stone-700 dark:text-stone-300">{k.client_id || '—'}</td>
                    <td className="py-2.5 text-center">
                      {k.is_external ? (
                        <span className="text-emerald-500">&#10003;</span>
                      ) : (
                        <span className="text-stone-300 dark:text-stone-600">&#10005;</span>
                      )}
                    </td>
                    <td className="py-2.5">
                      <div className="flex flex-wrap gap-1">
                        {k.scopes.map((s) => (
                          <span key={s} className="rounded-full bg-stone-100 px-2 py-0.5 text-[11px] text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                            {s}
                          </span>
                        ))}
                      </div>
                    </td>
                    <td className="py-2.5 text-xs text-stone-400">
                      {k.created_at ? new Date(k.created_at).toLocaleDateString() : '—'}
                    </td>
                    <td className="py-2.5 text-right">
                      <button
                        onClick={() => setRevoking(k.masked)}
                        className="inline-flex items-center gap-1 rounded px-2 py-1 text-xs text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-900/20"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                        {intl.formatMessage({ id: 'mcpKeys.revoke' })}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <CreateKeyDialog
        open={showCreate}
        onClose={() => setShowCreate(false)}
        onCreated={(res) => {
          setShowCreate(false);
          setCreated(res);
          fetchKeys();
        }}
      />

      <RevealKeyDialog result={created} onClose={() => setCreated(null)} />

      <RevokeKeyDialog
        masked={revoking}
        onClose={() => setRevoking(null)}
        onRevoked={fetchKeys}
      />
    </div>
  );
}

function CreateKeyDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: (res: McpKeyCreateResult) => void;
}) {
  const intl = useIntl();
  const [clientId, setClientId] = useState('');
  const [isExternal, setIsExternal] = useState(false);
  const [env, setEnv] = useState<'prod' | 'staging' | 'dev'>('prod');
  const [scopes, setScopes] = useState<McpScope[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reset = () => {
    setClientId('');
    setIsExternal(false);
    setEnv('prod');
    setScopes([]);
    setError(null);
  };

  const toggleScope = (s: McpScope) => {
    setScopes((prev) => (prev.includes(s) ? prev.filter((x) => x !== s) : [...prev, s]));
  };

  const handleSubmit = async () => {
    if (!clientId.trim() || scopes.length === 0) return;
    setSubmitting(true);
    setError(null);
    try {
      const res = await api.mcpKeys.create({ client_id: clientId.trim(), is_external: isExternal, scopes, env });
      onCreated(res);
      reset();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setSubmitting(false);
    }
  };

  const handleClose = () => {
    reset();
    onClose();
  };

  return (
    <Dialog open={open} onClose={handleClose} title={intl.formatMessage({ id: 'mcpKeys.create' })}>
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'mcpKeys.clientId' })} hint={intl.formatMessage({ id: 'mcpKeys.clientId.hint' })}>
          <input type="text" value={clientId} onChange={(e) => setClientId(e.target.value)} placeholder="my-integration" className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'mcpKeys.env' })}>
          <select value={env} onChange={(e) => setEnv(e.target.value as 'prod' | 'staging' | 'dev')} className={inputClass}>
            <option value="prod">prod</option>
            <option value="staging">staging</option>
            <option value="dev">dev</option>
          </select>
        </FormField>
        <label className="flex items-center justify-between py-1.5">
          <span className="text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'mcpKeys.external' })}</span>
          <button
            type="button"
            role="switch"
            aria-checked={isExternal}
            onClick={() => setIsExternal((v) => !v)}
            className={cn('relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full transition-colors', isExternal ? 'bg-amber-500' : 'bg-stone-300 dark:bg-stone-600')}
          >
            <span className={cn('pointer-events-none inline-block h-4 w-4 rounded-full bg-white shadow-sm transition-transform mt-0.5', isExternal ? 'translate-x-4 ml-0.5' : 'translate-x-0.5')} />
          </button>
        </label>
        <FormField label={intl.formatMessage({ id: 'mcpKeys.scopes' })} hint={intl.formatMessage({ id: 'mcpKeys.scopes.hint' })}>
          <div className="grid grid-cols-2 gap-2">
            {MCP_SCOPES.map((s) => (
              <label key={s} className="flex items-center gap-2 text-sm text-stone-700 dark:text-stone-300">
                <input type="checkbox" checked={scopes.includes(s)} onChange={() => toggleScope(s)} className="accent-amber-500" />
                <code className="text-xs">{s}</code>
              </label>
            ))}
          </div>
        </FormField>
        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={handleClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button onClick={handleSubmit} disabled={submitting || !clientId.trim() || scopes.length === 0} className={buttonPrimary}>
            {submitting ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'mcpKeys.create' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function RevealKeyDialog({ result, onClose }: { result: McpKeyCreateResult | null; onClose: () => void }) {
  const intl = useIntl();
  const [copied, setCopied] = useState(false);

  if (!result) return null;

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(result.key);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  return (
    <Dialog open={result !== null} onClose={onClose} title={intl.formatMessage({ id: 'mcpKeys.created.title' })}>
      <div className="space-y-4">
        <div className="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-900/20">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-500" />
          <p className="text-sm text-amber-700 dark:text-amber-400">{intl.formatMessage({ id: 'mcpKeys.created.warning' })}</p>
        </div>
        <div className="flex items-center gap-2">
          <code className="flex-1 break-all rounded-lg bg-stone-900 px-3 py-2 font-mono text-xs text-emerald-400">{result.key}</code>
          <button onClick={handleCopy} className={buttonSecondary}>
            {copied ? <Check className="h-4 w-4 text-emerald-500" /> : <Copy className="h-4 w-4" />}
            {copied ? intl.formatMessage({ id: 'mcpKeys.copied' }) : intl.formatMessage({ id: 'mcpKeys.copy' })}
          </button>
        </div>
        <div className="flex justify-end pt-2">
          <button onClick={onClose} className={buttonPrimary}>{intl.formatMessage({ id: 'mcpKeys.created.done' })}</button>
        </div>
      </div>
    </Dialog>
  );
}

function RevokeKeyDialog({
  masked,
  onClose,
  onRevoked,
}: {
  masked: string | null;
  onClose: () => void;
  onRevoked: () => void;
}) {
  const intl = useIntl();
  const [confirming, setConfirming] = useState(false);
  // The revoke RPC deletes by the full cleartext key (the config table key),
  // but `mcp_keys.list` only exposes a masked preview. The dashboard never
  // stores cleartext, so the operator must paste the full key to revoke it.
  const [fullKey, setFullKey] = useState('');

  if (!masked) return null;

  const handleConfirm = async () => {
    if (!fullKey.trim()) return;
    setConfirming(true);
    try {
      await api.mcpKeys.revoke(fullKey.trim());
      toast.success(intl.formatMessage({ id: 'mcpKeys.revoked' }));
      setFullKey('');
      onRevoked();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setConfirming(false);
    }
  };

  const handleClose = () => {
    setFullKey('');
    onClose();
  };

  return (
    <Dialog open={masked !== null} onClose={handleClose} title={intl.formatMessage({ id: 'mcpKeys.revoke.title' })}>
      <div className="space-y-4">
        <p className="text-sm text-stone-600 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.revoke.confirm' })}</p>
        <code className="block rounded bg-stone-100 px-2 py-1 font-mono text-xs text-stone-700 dark:bg-stone-800 dark:text-stone-300">{masked}</code>
        <FormField label={intl.formatMessage({ id: 'mcpKeys.revoke.fullKey' })} hint={intl.formatMessage({ id: 'mcpKeys.revoke.fullKey.hint' })}>
          <input type="text" value={fullKey} onChange={(e) => setFullKey(e.target.value)} placeholder="ddc_prod_..." className={cn(inputClass, 'font-mono')} autoComplete="off" />
        </FormField>
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={handleClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button onClick={handleConfirm} disabled={confirming || !fullKey.trim()} className="inline-flex items-center justify-center gap-2 rounded-lg bg-rose-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-rose-600 disabled:opacity-50">
            {confirming ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'mcpKeys.revoke' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
