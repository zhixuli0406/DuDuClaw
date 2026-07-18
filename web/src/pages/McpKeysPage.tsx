import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import {
  api,
  MCP_SCOPES,
  type McpKeyEntry,
  type McpKeyCreateResult,
  type McpScope,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Badge,
  Input,
  Switch,
  Checkbox,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Empty,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogClose,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
} from '@/components/mds';
import { KeyRound, Plus, Trash2, Copy, Check, AlertTriangle } from 'lucide-react';

/** Column template shared by the MCP-keys ListGrid header + rows (spec §4). */
const KEY_COLUMNS =
  'minmax(0,1.6fr) minmax(0,1fr) 4rem minmax(0,1.4fr) minmax(0,0.8fr) auto';

/**
 * McpKeysPage — MCP API keys tab of `/manage/integrations` (MDS surface). A slim
 * header (description + create action) over a Linear-style ListGrid of issued
 * keys. Create is an MDS Dialog; the one-time reveal + paste-to-revoke flows are
 * preserved verbatim. Same `mcp_keys.*` RPCs — no backend change.
 */
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
      {/* Slim tab header — description left, create action right. */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.desc' })}</p>
        <Button variant="brand" size="sm" onClick={() => setShowCreate(true)}>
          <Plus />
          {intl.formatMessage({ id: 'mcpKeys.create' })}
        </Button>
      </div>

      {loading ? (
        <p className="py-12 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : keys.length === 0 ? (
        <Empty icon={KeyRound} title={intl.formatMessage({ id: 'mcpKeys.empty' })} variant="dashed" />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={KEY_COLUMNS}
            className="!h-auto"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.key' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.clientId' })}</ListGridHeaderCell>
                <ListGridHeaderCell className="justify-center">
                  {intl.formatMessage({ id: 'mcpKeys.col.external' })}
                </ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.scopes' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.created' })}</ListGridHeaderCell>
                <ListGridHeaderCell className="justify-end" aria-hidden />
              </ListGridHeader>
            }
          >
            {keys.map((k) => (
              <ListGridRow key={k.masked} className="cursor-default" rowSize="lg">
                <ListGridCell className="gap-2">
                  <code className="truncate rounded bg-muted px-2 py-0.5 font-mono text-xs" title={k.masked}>
                    {k.masked}
                  </code>
                  {k.rotate_recommended && (
                    <Badge variant="destructive">
                      <AlertTriangle />
                      {intl.formatMessage({ id: 'mcpKeys.rotate' })}
                    </Badge>
                  )}
                </ListGridCell>
                <ListGridCell>
                  <span className="truncate text-sm text-muted-foreground" title={k.client_id || undefined}>
                    {k.client_id || '—'}
                  </span>
                </ListGridCell>
                <ListGridCell className="justify-center">
                  {k.is_external ? (
                    <Check className="size-4 text-success" />
                  ) : (
                    <span className="text-muted-foreground/40">—</span>
                  )}
                </ListGridCell>
                <ListGridCell>
                  <div className="flex flex-wrap gap-1">
                    {k.scopes.map((s) => (
                      <Badge key={s} variant="secondary" className="font-mono">
                        {s}
                      </Badge>
                    ))}
                  </div>
                </ListGridCell>
                <ListGridCell>
                  <span className="truncate text-xs text-muted-foreground">
                    {k.created_at ? new Date(k.created_at).toLocaleDateString() : '—'}
                  </span>
                </ListGridCell>
                <ListGridCell className="justify-end" data-stop-row-nav>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="text-destructive hover:bg-destructive/10"
                    onClick={() => setRevoking(k.masked)}
                  >
                    <Trash2 />
                    {intl.formatMessage({ id: 'mcpKeys.revoke' })}
                  </Button>
                </ListGridCell>
              </ListGridRow>
            ))}
          </ListGridContainer>
        </div>
      )}

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

      <RevokeKeyDialog masked={revoking} onClose={() => setRevoking(null)} onRevoked={fetchKeys} />
    </div>
  );
}

const ENV_OPTIONS = ['prod', 'staging', 'dev'] as const;
type KeyEnv = (typeof ENV_OPTIONS)[number];

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
  const [env, setEnv] = useState<KeyEnv>('prod');
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
    <Dialog open={open} onOpenChange={(o) => !o && handleClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcpKeys.create' })}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="mcpkey-client-id" className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.clientId' })}
            </label>
            <Input
              id="mcpkey-client-id"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              placeholder="my-integration"
            />
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.clientId.hint' })}</p>
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.env' })}
            </label>
            <Select value={env} onValueChange={(v) => setEnv(String(v) as KeyEnv)}>
              <SelectTrigger className="w-full" aria-label={intl.formatMessage({ id: 'mcpKeys.env' })}>
                <SelectValue>{env}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {ENV_OPTIONS.map((o) => (
                  <SelectItem key={o} value={o}>
                    {o}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <label className="flex items-center justify-between gap-4">
            <span className="text-sm">{intl.formatMessage({ id: 'mcpKeys.external' })}</span>
            <Switch
              checked={isExternal}
              onCheckedChange={(v) => setIsExternal(Boolean(v))}
              aria-label={intl.formatMessage({ id: 'mcpKeys.external' })}
            />
          </label>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.scopes' })}
            </label>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.scopes.hint' })}</p>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              {MCP_SCOPES.map((s) => (
                <label key={s} className="flex items-start gap-2">
                  <Checkbox
                    checked={scopes.includes(s)}
                    onCheckedChange={() => toggleScope(s)}
                    className="mt-0.5"
                    aria-label={s}
                  />
                  <span className="min-w-0">
                    <code className="text-xs">{s}</code>
                    <span className="mt-0.5 block text-xs text-muted-foreground">
                      {intl.formatMessage({ id: `mcpKeys.scopeDesc.${s.replace(':', '.')}` })}
                    </span>
                  </span>
                </label>
              ))}
            </div>
          </div>

          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>

        <DialogFooter>
          <DialogClose
            render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>}
          />
          <Button
            variant="brand"
            onClick={handleSubmit}
            disabled={submitting || !clientId.trim() || scopes.length === 0}
          >
            {submitting ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'mcpKeys.create' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function RevealKeyDialog({ result, onClose }: { result: McpKeyCreateResult | null; onClose: () => void }) {
  const intl = useIntl();
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.key);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  return (
    <Dialog open={result !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcpKeys.created.title' })}</DialogTitle>
        </DialogHeader>

        {result && (
          <div className="space-y-4">
            <div className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 p-3">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
              <p className="text-sm">{intl.formatMessage({ id: 'mcpKeys.created.warning' })}</p>
            </div>
            <div className="flex items-center gap-2">
              <code className="flex-1 break-all rounded-lg bg-stone-900 px-3 py-2 font-mono text-xs text-emerald-400">
                {result.key}
              </code>
              <Button variant="outline" onClick={handleCopy}>
                {copied ? <Check className="text-success" /> : <Copy />}
                {copied ? intl.formatMessage({ id: 'mcpKeys.copied' }) : intl.formatMessage({ id: 'mcpKeys.copy' })}
              </Button>
            </div>
          </div>
        )}

        <DialogFooter>
          <Button variant="brand" onClick={onClose}>
            {intl.formatMessage({ id: 'mcpKeys.created.done' })}
          </Button>
        </DialogFooter>
      </DialogContent>
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
    <Dialog open={masked !== null} onOpenChange={(o) => !o && handleClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcpKeys.revoke.title' })}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.revoke.confirm' })}</p>
          {masked && (
            <code className="block rounded bg-muted px-2 py-1 font-mono text-xs">{masked}</code>
          )}
          <div className="space-y-1.5">
            <label htmlFor="mcpkey-full-key" className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.revoke.fullKey' })}
            </label>
            <Input
              id="mcpkey-full-key"
              value={fullKey}
              onChange={(e) => setFullKey(e.target.value)}
              placeholder="ddc_prod_..."
              className="font-mono"
              autoComplete="off"
            />
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.revoke.fullKey.hint' })}
            </p>
          </div>
        </div>

        <DialogFooter>
          <DialogClose
            render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>}
          />
          <Button variant="destructive" onClick={handleConfirm} disabled={confirming || !fullKey.trim()}>
            {confirming ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'mcpKeys.revoke' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
