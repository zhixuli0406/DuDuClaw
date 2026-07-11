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
import { Dialog } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import { Page, PageHeader, Card, Button, Badge, EmptyState, Field, controlClass } from '@/components/ui';
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
    <Page>
      <PageHeader
        icon={KeyRound}
        title={intl.formatMessage({ id: 'nav.mcpKeys' })}
        subtitle={intl.formatMessage({ id: 'mcpKeys.desc' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setShowCreate(true)}>
            {intl.formatMessage({ id: 'mcpKeys.create' })}
          </Button>
        }
      />

      <Card padded={false}>
        {loading ? (
          <p className="py-12 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
        ) : keys.length === 0 ? (
          <EmptyState
            icon={KeyRound}
            title={intl.formatMessage({ id: 'mcpKeys.empty' })}
          />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)]">
                  <th className="px-5 py-3 text-left text-xs font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.key' })}</th>
                  <th className="px-5 py-3 text-left text-xs font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.clientId' })}</th>
                  <th className="px-5 py-3 text-center text-xs font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.external' })}</th>
                  <th className="px-5 py-3 text-left text-xs font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.scopes' })}</th>
                  <th className="px-5 py-3 text-left text-xs font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'mcpKeys.col.created' })}</th>
                  <th className="px-5 py-3 text-right text-xs font-medium text-stone-500 dark:text-stone-400" />
                </tr>
              </thead>
              <tbody className="divide-y divide-[var(--panel-border)]">
                {keys.map((k) => (
                  <tr key={k.masked}>
                    <td className="px-5 py-2.5">
                      <div className="flex flex-wrap items-center gap-2">
                        <code className="rounded bg-stone-500/10 px-2 py-0.5 font-mono text-xs text-stone-700 dark:text-stone-300">
                          {k.masked}
                        </code>
                        {k.rotate_recommended && (
                          <Badge tone="warning">
                            <AlertTriangle className="h-3 w-3" />
                            {intl.formatMessage({ id: 'mcpKeys.rotate' })}
                          </Badge>
                        )}
                      </div>
                    </td>
                    <td className="px-5 py-2.5 text-stone-700 dark:text-stone-300">{k.client_id || '—'}</td>
                    <td className="px-5 py-2.5 text-center">
                      {k.is_external ? (
                        <span className="text-emerald-500">&#10003;</span>
                      ) : (
                        <span className="text-stone-300 dark:text-stone-600">&#10005;</span>
                      )}
                    </td>
                    <td className="px-5 py-2.5">
                      <div className="flex flex-wrap gap-1">
                        {k.scopes.map((s) => (
                          <Badge key={s} tone="neutral">
                            <span className="font-mono">{s}</span>
                          </Badge>
                        ))}
                      </div>
                    </td>
                    <td className="px-5 py-2.5 text-xs text-stone-400">
                      {k.created_at ? new Date(k.created_at).toLocaleDateString() : '—'}
                    </td>
                    <td className="px-5 py-2.5 text-right">
                      <Button variant="ghost" size="sm" icon={Trash2} onClick={() => setRevoking(k.masked)} className="text-rose-600 hover:bg-rose-500/10 hover:text-rose-700 dark:text-rose-400">
                        {intl.formatMessage({ id: 'mcpKeys.revoke' })}
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

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
    </Page>
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
        <Field label={intl.formatMessage({ id: 'mcpKeys.clientId' })} help={intl.formatMessage({ id: 'mcpKeys.clientId.hint' })}>
          <input type="text" value={clientId} onChange={(e) => setClientId(e.target.value)} placeholder="my-integration" className={controlClass} />
        </Field>
        <Field label={intl.formatMessage({ id: 'mcpKeys.env' })}>
          <select value={env} onChange={(e) => setEnv(e.target.value as 'prod' | 'staging' | 'dev')} className={controlClass}>
            <option value="prod">prod</option>
            <option value="staging">staging</option>
            <option value="dev">dev</option>
          </select>
        </Field>
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
        <Field label={intl.formatMessage({ id: 'mcpKeys.scopes' })} help={intl.formatMessage({ id: 'mcpKeys.scopes.hint' })}>
          <div className="grid grid-cols-2 gap-2">
            {MCP_SCOPES.map((s) => (
              <label key={s} className="flex items-start gap-2 text-sm text-stone-700 dark:text-stone-300">
                <input type="checkbox" checked={scopes.includes(s)} onChange={() => toggleScope(s)} className="mt-0.5 accent-amber-500" />
                <span className="min-w-0">
                  <code className="text-xs">{s}</code>
                  <span className="mt-0.5 block text-xs text-stone-400 dark:text-stone-500">
                    {intl.formatMessage({ id: `mcpKeys.scopeDesc.${s.replace(':', '.')}` })}
                  </span>
                </span>
              </label>
            ))}
          </div>
        </Field>
        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 pt-2">
          <Button variant="secondary" onClick={handleClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button variant="primary" onClick={handleSubmit} disabled={submitting || !clientId.trim() || scopes.length === 0}>
            {submitting ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'mcpKeys.create' })}
          </Button>
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
          <Button variant="secondary" onClick={handleCopy} icon={copied ? Check : Copy} className={copied ? '[&_svg]:text-emerald-500' : undefined}>
            {copied ? intl.formatMessage({ id: 'mcpKeys.copied' }) : intl.formatMessage({ id: 'mcpKeys.copy' })}
          </Button>
        </div>
        <div className="flex justify-end pt-2">
          <Button variant="primary" onClick={onClose}>{intl.formatMessage({ id: 'mcpKeys.created.done' })}</Button>
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
        <code className="block rounded bg-stone-500/10 px-2 py-1 font-mono text-xs text-stone-700 dark:text-stone-300">{masked}</code>
        <Field label={intl.formatMessage({ id: 'mcpKeys.revoke.fullKey' })} help={intl.formatMessage({ id: 'mcpKeys.revoke.fullKey.hint' })}>
          <input type="text" value={fullKey} onChange={(e) => setFullKey(e.target.value)} placeholder="ddc_prod_..." className={cn(controlClass, 'font-mono')} autoComplete="off" />
        </Field>
        <div className="flex justify-end gap-3 pt-2">
          <Button variant="secondary" onClick={handleClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button variant="danger" onClick={handleConfirm} disabled={confirming || !fullKey.trim()}>
            {confirming ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'mcpKeys.revoke' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
