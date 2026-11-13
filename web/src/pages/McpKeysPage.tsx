import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import {
  api,
  type McpKeyEntry,
  type McpKeyCreateResult,
  type McpKeyListResult,
} from '@/lib/api';
import {
  DEFAULT_EXPIRY,
  EXPIRY_CHOICES,
  expiryPayload,
  tomorrow,
  type ExpiryChoice,
  type ExpiryState,
} from '@/lib/mcp-key-expiry';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Badge,
  Input,
  Checkbox,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Empty,
  ErrorState,
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
import { OAuthGrantsPanel } from '@/components/mcp/OAuthGrantsPanel';
import {
  KeyRound,
  Plus,
  Trash2,
  Copy,
  Check,
  AlertTriangle,
  Pencil,
  CalendarClock,
  Ban,
  Power,
  Info,
} from 'lucide-react';

/** Column template shared by the MCP-keys ListGrid header + rows (spec §4). */
const KEY_COLUMNS =
  'minmax(0,1.8fr) minmax(0,1fr) minmax(0,0.9fr) minmax(0,1fr) minmax(0,0.9fr) 6rem auto';

/** Longest key name (mirrors `duduclaw_core::mcp_key_store::NAME_MAX_CHARS`). */
const NAME_MAX_CHARS = 60;

/**
 * McpKeysPage — MCP API keys tab of `/manage/integrations` (MDS surface).
 *
 * RM-134 (v1.82): keys live in the hashed key store and are managed by id —
 * name, creator, created, expires, last used and status per row; rename,
 * change expiry, one-click disable / enable and delete (with confirm) as row
 * actions. The plaintext key is shown once, in the reveal dialog after
 * creation, and never again.
 */
export function McpKeysPage() {
  const intl = useIntl();
  const [list, setList] = useState<McpKeyListResult | null>(null);
  const [loading, setLoading] = useState(false);
  // P05: with no list a failed call must not render the "no keys yet" empty
  // state — issuing a second key because the first looked missing is exactly
  // the wrong recovery.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [created, setCreated] = useState<McpKeyCreateResult | null>(null);
  const [renaming, setRenaming] = useState<McpKeyEntry | null>(null);
  const [changingExpiry, setChangingExpiry] = useState<McpKeyEntry | null>(null);
  const [deleting, setDeleting] = useState<McpKeyEntry | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const keys = list?.keys ?? [];

  const fetchKeys = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const res = await api.mcpKeys.list();
      setList(res ?? null);
    } catch (e) {
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    fetchKeys();
  }, [fetchKeys]);

  const toggleDisabled = async (key: McpKeyEntry) => {
    setBusyId(key.id);
    try {
      if (key.status === 'disabled') {
        await api.mcpKeys.enable(key.id);
        toast.success(intl.formatMessage({ id: 'mcpKeys.enabled' }, { name: key.name }));
      } else {
        await api.mcpKeys.disable(key.id);
        toast.success(intl.formatMessage({ id: 'mcpKeys.disabled' }, { name: key.name }));
      }
      await fetchKeys();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setBusyId(null);
    }
  };

  const dismissHint = async () => {
    try {
      await api.mcpKeys.dismissMigrationHint();
      await fetchKeys();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  const expiringSoon = keys.filter((k) => k.expiring_soon);
  const date = (iso: string | null) =>
    iso ? intl.formatDate(iso, { year: 'numeric', month: 'short', day: 'numeric' }) : '—';
  const dateTime = (iso: string | null) =>
    iso
      ? intl.formatDate(iso, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
      : intl.formatMessage({ id: 'mcpKeys.neverUsed' });

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

      {list?.migration_hint && (
        <div
          role="status"
          className="flex flex-wrap items-start justify-between gap-3 rounded-lg border border-warning/30 bg-warning/10 p-3"
        >
          <div className="flex min-w-0 items-start gap-2">
            <Info className="mt-0.5 size-4 shrink-0 text-warning" />
            <p className="text-sm">{intl.formatMessage({ id: 'mcpKeys.migrationHint' })}</p>
          </div>
          <Button variant="outline" size="sm" onClick={() => void dismissHint()}>
            {intl.formatMessage({ id: 'mcpKeys.migrationHint.dismiss' })}
          </Button>
        </div>
      )}

      {expiringSoon.length > 0 && (
        <div role="alert" className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 p-3">
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
          <p className="text-sm">
            {intl.formatMessage(
              { id: 'mcpKeys.expiringBanner' },
              { count: expiringSoon.length, names: expiringSoon.map((k) => k.name).join('、') },
            )}
          </p>
        </div>
      )}

      {(list?.legacy_pending ?? 0) > 0 && (
        <div role="alert" className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 p-3">
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
          <p className="text-sm">
            {intl.formatMessage({ id: 'mcpKeys.legacyPending' }, { count: list?.legacy_pending ?? 0 })}
          </p>
        </div>
      )}

      {(list?.ignored_legacy_entries?.length ?? 0) > 0 && (
        <div role="alert" className="flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/10 p-3">
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
          <p className="text-sm">
            {intl.formatMessage(
              { id: 'mcpKeys.ignoredLegacy' },
              {
                count: list?.ignored_legacy_entries.length ?? 0,
                hints: (list?.ignored_legacy_entries ?? []).join('、'),
              },
            )}
          </p>
        </div>
      )}

      {loading && list === null ? (
        <p className="py-12 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : loadError != null && keys.length === 0 ? (
        <ErrorState
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
          onRetry={() => void fetchKeys()}
        />
      ) : keys.length === 0 ? (
        <Empty icon={KeyRound} title={intl.formatMessage({ id: 'mcpKeys.empty' })} variant="dashed" />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={KEY_COLUMNS}
            className="!h-auto"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.name' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.creator' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.created' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.expires' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.lastUsed' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'mcpKeys.col.status' })}</ListGridHeaderCell>
                <ListGridHeaderCell className="justify-end" aria-hidden />
              </ListGridHeader>
            }
          >
            {keys.map((k) => (
              <ListGridRow key={k.id} className="cursor-default" rowSize="lg">
                <ListGridCell className="flex-col !items-start gap-1">
                  <span className="truncate text-sm font-medium" title={k.name}>
                    {k.name}
                  </span>
                  <span className="flex flex-wrap items-center gap-1 text-xs text-muted-foreground">
                    <code className="rounded bg-muted px-1.5 py-0.5 font-mono">{k.hint}</code>
                    <span className="truncate" title={k.client_id}>
                      {k.client_id}
                    </span>
                    {k.is_external && (
                      <Badge variant="outline">{intl.formatMessage({ id: 'mcpKeys.col.external' })}</Badge>
                    )}
                    {k.employee_missing && (
                      <Badge variant="destructive">{intl.formatMessage({ id: 'mcpKeys.employeeMissing' })}</Badge>
                    )}
                    {k.needs_reissue && (
                      <Badge variant="destructive">{intl.formatMessage({ id: 'mcpKeys.needsReissue' })}</Badge>
                    )}
                    {k.scopes.map((s) => (
                      <Badge
                        key={s}
                        variant={(k.ineffective_scopes ?? []).includes(s) ? 'outline' : 'secondary'}
                        className={(k.ineffective_scopes ?? []).includes(s) ? 'font-mono line-through' : 'font-mono'}
                      >
                        {s}
                      </Badge>
                    ))}
                  </span>
                  {(k.ineffective_scopes ?? []).length > 0 && (
                    <span className="text-xs text-warning">
                      {intl.formatMessage({ id: 'mcpKeys.ineffective' }, { scopes: k.ineffective_scopes.join(', ') })}
                    </span>
                  )}
                </ListGridCell>
                <ListGridCell>
                  <span className="truncate text-sm text-muted-foreground">
                    {k.created_by_name ??
                      k.created_by ??
                      intl.formatMessage({ id: k.migrated ? 'mcpKeys.creator.migrated' : 'mcpKeys.creator.unknown' })}
                  </span>
                </ListGridCell>
                <ListGridCell>
                  <span className="truncate text-xs text-muted-foreground">{date(k.created_at)}</span>
                </ListGridCell>
                <ListGridCell className="flex-col !items-start gap-0.5">
                  <span className="truncate text-xs">
                    {k.expires_at ? date(k.expires_at) : intl.formatMessage({ id: 'mcpKeys.expiry.none' })}
                  </span>
                  {k.expiring_soon && k.days_left != null && (
                    <Badge variant="destructive">
                      <AlertTriangle />
                      {intl.formatMessage({ id: 'mcpKeys.expiresIn' }, { days: Math.max(k.days_left, 0) })}
                    </Badge>
                  )}
                  {k.migrated && !k.expiry_reviewed && (
                    <span className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.migrated' })}</span>
                  )}
                </ListGridCell>
                <ListGridCell>
                  <span className="truncate text-xs text-muted-foreground">{dateTime(k.last_used_at)}</span>
                </ListGridCell>
                <ListGridCell>
                  <Badge
                    variant={k.status === 'active' ? 'secondary' : k.status === 'expired' ? 'destructive' : 'outline'}
                  >
                    {intl.formatMessage({ id: `mcpKeys.status.${k.status}` })}
                  </Badge>
                </ListGridCell>
                <ListGridCell className="justify-end gap-1" data-stop-row-nav>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={intl.formatMessage({ id: 'mcpKeys.action.rename' }, { name: k.name })}
                    title={intl.formatMessage({ id: 'mcpKeys.action.rename' }, { name: k.name })}
                    onClick={() => setRenaming(k)}
                  >
                    <Pencil />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={intl.formatMessage({ id: 'mcpKeys.action.expiry' }, { name: k.name })}
                    title={intl.formatMessage({ id: 'mcpKeys.action.expiry' }, { name: k.name })}
                    onClick={() => setChangingExpiry(k)}
                  >
                    <CalendarClock />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    disabled={busyId === k.id}
                    aria-label={intl.formatMessage(
                      { id: k.status === 'disabled' ? 'mcpKeys.action.enable' : 'mcpKeys.action.disable' },
                      { name: k.name },
                    )}
                    title={intl.formatMessage(
                      { id: k.status === 'disabled' ? 'mcpKeys.action.enable' : 'mcpKeys.action.disable' },
                      { name: k.name },
                    )}
                    onClick={() => void toggleDisabled(k)}
                  >
                    {k.status === 'disabled' ? <Power /> : <Ban />}
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-destructive hover:bg-destructive/10"
                    aria-label={intl.formatMessage({ id: 'mcpKeys.action.delete' }, { name: k.name })}
                    title={intl.formatMessage({ id: 'mcpKeys.action.delete' }, { name: k.name })}
                    onClick={() => setDeleting(k)}
                  >
                    <Trash2 />
                  </Button>
                </ListGridCell>
              </ListGridRow>
            ))}
          </ListGridContainer>
        </div>
      )}

      <OAuthGrantsPanel />

      <CreateKeyDialog
        open={showCreate}
        effectiveScopes={list?.effective_scopes ?? []}
        employeeScopes={list?.employee_key_scopes ?? []}
        onClose={() => setShowCreate(false)}
        onCreated={(res) => {
          setShowCreate(false);
          setCreated(res);
          fetchKeys();
        }}
      />

      <RevealKeyDialog result={created} onClose={() => setCreated(null)} />

      <RenameKeyDialog keyEntry={renaming} onClose={() => setRenaming(null)} onDone={fetchKeys} />

      <ExpiryKeyDialog keyEntry={changingExpiry} onClose={() => setChangingExpiry(null)} onDone={fetchKeys} />

      <DeleteKeyDialog keyEntry={deleting} onClose={() => setDeleting(null)} onDone={fetchKeys} />
    </div>
  );
}

// ── Expiry choice (create + change) ──────────────────────────────────────────

function ExpiryPicker({ value, onChange, idPrefix }: { value: ExpiryState; onChange: (v: ExpiryState) => void; idPrefix: string }) {
  const intl = useIntl();
  const label = (c: ExpiryChoice) => intl.formatMessage({ id: `mcpKeys.expiry.${c}` });
  return (
    <div className="space-y-2">
      <label className="text-xs font-medium text-muted-foreground" htmlFor={`${idPrefix}-expiry`}>
        {intl.formatMessage({ id: 'mcpKeys.expiry' })}
      </label>
      <Select
        value={value.choice}
        onValueChange={(v) => onChange({ ...value, choice: String(v) as ExpiryChoice, confirmed: false })}
      >
        <SelectTrigger id={`${idPrefix}-expiry`} className="w-full" aria-label={intl.formatMessage({ id: 'mcpKeys.expiry' })}>
          <SelectValue>{label(value.choice)}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {EXPIRY_CHOICES.map((c) => (
            <SelectItem key={c} value={c}>
              {label(c)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {value.choice === 'custom' && (
        <Input
          type="date"
          min={tomorrow()}
          value={value.date}
          onChange={(e) => onChange({ ...value, date: e.target.value })}
          aria-label={intl.formatMessage({ id: 'mcpKeys.expiry.customDate' })}
        />
      )}
      {value.choice === 'never' && (
        <div className="space-y-2 rounded-lg border border-destructive/30 bg-destructive/10 p-3">
          <div className="flex items-start gap-2">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
            <p className="text-sm">{intl.formatMessage({ id: 'mcpKeys.expiry.neverRisk' })}</p>
          </div>
          <label className="flex items-start gap-2 text-sm">
            <Checkbox
              checked={value.confirmed}
              onCheckedChange={(v) => onChange({ ...value, confirmed: Boolean(v) })}
              className="mt-0.5"
              aria-label={intl.formatMessage({ id: 'mcpKeys.expiry.neverConfirm' })}
            />
            <span>{intl.formatMessage({ id: 'mcpKeys.expiry.neverConfirm' })}</span>
          </label>
        </div>
      )}
    </div>
  );
}

// ── Create ───────────────────────────────────────────────────────────────────

const ENV_OPTIONS = ['prod', 'staging', 'dev'] as const;
type KeyEnv = (typeof ENV_OPTIONS)[number];

/** RM-134 H3: a key for an outside integration is always external; an AI
 *  employee's own key (internal, never `admin`) is requested by employee id. */
const KEY_KINDS = ['integration', 'employee'] as const;
type KeyKind = (typeof KEY_KINDS)[number];

function CreateKeyDialog({
  open,
  effectiveScopes,
  employeeScopes,
  onClose,
  onCreated,
}: {
  open: boolean;
  /** RM-157: the backend's list (`mcp_keys.list` → `effective_scopes`). */
  effectiveScopes: string[];
  /** `mcp_keys.list` → `employee_key_scopes`. */
  employeeScopes: string[];
  onClose: () => void;
  onCreated: (res: McpKeyCreateResult) => void;
}) {
  const intl = useIntl();
  const [name, setName] = useState('');
  const [kind, setKind] = useState<KeyKind>('integration');
  const [clientId, setClientId] = useState('');
  const [env, setEnv] = useState<KeyEnv>('prod');
  const [scopes, setScopes] = useState<string[]>([]);
  const [expiry, setExpiry] = useState<ExpiryState>(DEFAULT_EXPIRY);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reset = () => {
    setName('');
    setKind('integration');
    setClientId('');
    setEnv('prod');
    setScopes([]);
    setExpiry(DEFAULT_EXPIRY);
    setError(null);
  };

  const offered = kind === 'employee' ? employeeScopes : effectiveScopes;

  const toggleScope = (s: string) => {
    setScopes((prev) => (prev.includes(s) ? prev.filter((x) => x !== s) : [...prev, s]));
  };

  const payload = expiryPayload(expiry);
  const nameOk = name.trim().length > 0 && [...name.trim()].length <= NAME_MAX_CHARS;
  const chosen = scopes.filter((s) => offered.includes(s));
  const canSubmit = nameOk && clientId.trim().length > 0 && chosen.length > 0 && payload !== null;

  const handleSubmit = async () => {
    if (!canSubmit || payload === null) return;
    setSubmitting(true);
    setError(null);
    try {
      const target = kind === 'employee' ? { agent_id: clientId.trim() } : { client_id: clientId.trim() };
      const res = await api.mcpKeys.create({
        name: name.trim(),
        ...target,
        scopes: chosen,
        env,
        expiry: payload,
      });
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

        <div className="max-h-[70vh] space-y-4 overflow-y-auto">
          <div className="space-y-1.5">
            <label htmlFor="mcpkey-name" className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.name' })}
            </label>
            <Input
              id="mcpkey-name"
              value={name}
              maxLength={NAME_MAX_CHARS}
              onChange={(e) => setName(e.target.value)}
              placeholder={intl.formatMessage({ id: 'mcpKeys.name.placeholder' })}
            />
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.name.hint' })}</p>
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.kind' })}
            </label>
            <Select
              value={kind}
              onValueChange={(v) => {
                setKind(String(v) as KeyKind);
                setScopes([]);
              }}
            >
              <SelectTrigger className="w-full" aria-label={intl.formatMessage({ id: 'mcpKeys.kind' })}>
                <SelectValue>{intl.formatMessage({ id: `mcpKeys.kind.${kind}` })}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {KEY_KINDS.map((k) => (
                  <SelectItem key={k} value={k}>
                    {intl.formatMessage({ id: `mcpKeys.kind.${k}` })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-1.5">
            <label htmlFor="mcpkey-client-id" className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: kind === 'employee' ? 'mcpKeys.employee' : 'mcpKeys.clientId' })}
            </label>
            <Input
              id="mcpkey-client-id"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              placeholder={kind === 'employee' ? 'agnes' : 'my-integration'}
            />
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: kind === 'employee' ? 'mcpKeys.employee.hint' : 'mcpKeys.clientId.hint' })}
            </p>
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

          <ExpiryPicker value={expiry} onChange={setExpiry} idPrefix="mcpkey-create" />

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcpKeys.scopes' })}
            </label>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.scopes.hint' })}</p>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              {offered.map((s) => (
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
          <Button variant="brand" onClick={handleSubmit} disabled={submitting || !canSubmit}>
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
            <p className="text-sm font-medium">{result.key_info.name}</p>
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

// ── Row actions ──────────────────────────────────────────────────────────────

function RenameKeyDialog({
  keyEntry,
  onClose,
  onDone,
}: {
  keyEntry: McpKeyEntry | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const intl = useIntl();
  const [name, setName] = useState('');
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    setName(keyEntry?.name ?? '');
  }, [keyEntry]);
  const valid = name.trim().length > 0 && [...name.trim()].length <= NAME_MAX_CHARS;

  const save = async () => {
    if (!keyEntry || !valid) return;
    setSaving(true);
    try {
      await api.mcpKeys.rename(keyEntry.id, name.trim());
      toast.success(intl.formatMessage({ id: 'mcpKeys.renamed' }));
      onDone();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={keyEntry !== null} onOpenChange={(o) => !o && !saving && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcpKeys.rename.title' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-1.5">
          <label htmlFor="mcpkey-rename" className="text-xs font-medium text-muted-foreground">
            {intl.formatMessage({ id: 'mcpKeys.name' })}
          </label>
          <Input id="mcpkey-rename" value={name} maxLength={NAME_MAX_CHARS} onChange={(e) => setName(e.target.value)} />
        </div>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={saving}>{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="brand" onClick={() => void save()} disabled={saving || !valid}>
            {intl.formatMessage({ id: saving ? 'common.loading' : 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ExpiryKeyDialog({
  keyEntry,
  onClose,
  onDone,
}: {
  keyEntry: McpKeyEntry | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const intl = useIntl();
  const [expiry, setExpiry] = useState<ExpiryState>(DEFAULT_EXPIRY);
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    setExpiry(DEFAULT_EXPIRY);
  }, [keyEntry]);
  const payload = expiryPayload(expiry);

  const save = async () => {
    if (!keyEntry || payload === null) return;
    setSaving(true);
    try {
      await api.mcpKeys.setExpiry(keyEntry.id, payload);
      toast.success(intl.formatMessage({ id: 'mcpKeys.expiryChanged' }));
      onDone();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={keyEntry !== null} onOpenChange={(o) => !o && !saving && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcpKeys.expiry.title' }, { name: keyEntry?.name ?? '' })}</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage(
            { id: 'mcpKeys.expiry.current' },
            {
              date: keyEntry?.expires_at
                ? intl.formatDate(keyEntry.expires_at, { year: 'numeric', month: 'short', day: 'numeric' })
                : intl.formatMessage({ id: 'mcpKeys.expiry.none' }),
            },
          )}
        </p>
        <ExpiryPicker value={expiry} onChange={setExpiry} idPrefix="mcpkey-change" />
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={saving}>{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="brand" onClick={() => void save()} disabled={saving || payload === null}>
            {intl.formatMessage({ id: saving ? 'common.loading' : 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function DeleteKeyDialog({
  keyEntry,
  onClose,
  onDone,
}: {
  keyEntry: McpKeyEntry | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const intl = useIntl();
  const [deleting, setDeleting] = useState(false);

  const confirm = async () => {
    if (!keyEntry) return;
    setDeleting(true);
    try {
      await api.mcpKeys.remove(keyEntry.id);
      toast.success(intl.formatMessage({ id: 'mcpKeys.revoked' }));
      onDone();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setDeleting(false);
    }
  };

  return (
    <Dialog open={keyEntry !== null} onOpenChange={(o) => !o && !deleting && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcpKeys.revoke.title' })}</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'mcpKeys.delete.confirm' }, { name: keyEntry?.name ?? '' })}
        </p>
        {keyEntry && <code className="block rounded bg-muted px-2 py-1 font-mono text-xs">{keyEntry.hint}</code>}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={deleting}>{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="destructive" onClick={() => void confirm()} disabled={deleting}>
            {intl.formatMessage({ id: deleting ? 'common.loading' : 'mcpKeys.revoke' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
