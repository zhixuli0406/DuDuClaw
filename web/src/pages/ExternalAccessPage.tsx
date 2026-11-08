import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Check, Copy, Lock, Plus, TriangleAlert } from 'lucide-react';
import { api, type StatusTokenCreateResult, type StatusTokenInfo, type StatusTokenListResult } from '@/lib/api';
import {
  Button, Card, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
  Input, Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/mds';
import { toast } from '@/lib/toast';
import {
  RevokeConfirmDialog, formatTokenDate, formatTokenLastUsed, statusTokenErrorCode, statusTokenErrorMessage,
} from '@/components/external-status/shared';

const DATA_ROWS = ['inProgress', 'awaiting', 'doneToday', 'done7d', 'latest'] as const;

/** My account → External access: the caller's own read-only `ddc_status_` credentials (RM-064). */
export function ExternalAccessPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `externalStatus.${id}` }, values);
  const [data, setData] = useState<StatusTokenListResult | null>(null);
  const [failed, setFailed] = useState(false);
  const [creating, setCreating] = useState(false);
  // The one-time secret lives only in this state. Closing the dialog drops it;
  // it is never written to storage, the list, or the URL.
  const [created, setCreated] = useState<StatusTokenCreateResult | null>(null);
  const [revoking, setRevoking] = useState<StatusTokenInfo | null>(null);
  const [revokeBusy, setRevokeBusy] = useState(false);
  const [revokeError, setRevokeError] = useState<string | null>(null);
  const epoch = useRef(0);

  const load = useCallback(async () => {
    const current = ++epoch.current;
    try {
      const result = await api.statusTokens.list();
      if (current !== epoch.current) return;
      setData(result); setFailed(false);
    } catch {
      if (current === epoch.current) setFailed(true);
    }
  }, []);
  useEffect(() => { void load(); return () => { ++epoch.current; }; }, [load]);

  const tokens = data?.tokens ?? [];
  const activeCount = tokens.filter(token => !token.revoked_at && token.active).length;
  const max = data?.max_active ?? 5;
  const atLimit = data !== null && activeCount >= max;

  const confirmRevoke = async () => {
    if (!revoking) return;
    setRevokeBusy(true); setRevokeError(null);
    try {
      await api.statusTokens.revoke(revoking.id);
      setRevoking(null);
      toast.success(t('revoked'));
      await load();
    } catch (err) {
      setRevokeError(statusTokenErrorMessage(intl, err));
      if (statusTokenErrorCode(err) === 'status_token_not_found') void load();
    } finally { setRevokeBusy(false); }
  };

  return <div className="mx-auto w-full max-w-4xl space-y-5">
    <header className="flex flex-wrap items-center justify-between gap-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <h1 className="text-base font-medium">{t('page.title')}</h1>
        <span className="text-sm text-muted-foreground">{t('page.subtitle')}</span>
      </div>
      {data?.enabled && <Button size="sm" disabled={atLimit} aria-describedby={atLimit ? 'status-token-limit' : undefined} onClick={() => setCreating(true)}>
        <Plus />{t('create')}
      </Button>}
    </header>

    {failed && <div role="alert" className="flex flex-wrap items-center justify-between gap-3 rounded-lg bg-destructive/10 px-4 py-3 text-sm text-destructive">
      <span>{t('loadFailed')}</span>
      <Button variant="outline" size="sm" onClick={() => void load()}>{t('retry')}</Button>
    </div>}

    {!data && !failed && <p role="status" className="py-10 text-center text-sm text-muted-foreground">{t('loading')}</p>}

    {data && !data.enabled && <Card className="items-center gap-3 px-7 py-10 text-center">
      <span className="flex size-12 items-center justify-center rounded-full bg-muted text-muted-foreground"><Lock className="size-5" aria-hidden="true" /></span>
      <h2 className="text-base font-medium">{t('disabled.title')}</h2>
      <p className="max-w-md text-sm text-muted-foreground">{t('disabled.body')}</p>
      <Button disabled>{t('create')}</Button>
    </Card>}

    {data?.enabled && <>
      <p role="note" className="rounded-lg bg-muted px-4 py-3 text-sm leading-relaxed text-muted-foreground">{t('scopeNote')}</p>

      <Card className="gap-0 overflow-hidden p-0">
        <div className="flex items-center justify-between gap-3 px-4 py-3">
          <h2 className="text-sm font-medium">{t('mine')}</h2>
          <span className="text-xs text-muted-foreground">{t('activeCount', { active: activeCount, max })}</span>
        </div>
        {atLimit && <p id="status-token-limit" role="note" className="mx-4 mb-3 rounded-lg bg-muted px-3 py-2 text-sm text-muted-foreground">{t('limitHint', { max })}</p>}
        {tokens.length === 0
          ? <p className="border-t border-border px-4 py-8 text-center text-sm text-muted-foreground">{t('empty')}</p>
          : <div className="overflow-x-auto border-t border-border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t('col.name')}</TableHead>
                  <TableHead>{t('col.created')}</TableHead>
                  <TableHead>{t('col.lastUsed')}</TableHead>
                  <TableHead>{t('col.status')}</TableHead>
                  <TableHead><span className="sr-only">{t('revoke')}</span></TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {tokens.map(token => {
                  const revoked = Boolean(token.revoked_at);
                  return <TableRow key={token.id}>
                    <TableCell className={revoked ? 'text-muted-foreground' : 'font-medium'}>{token.name}</TableCell>
                    <TableCell className="text-muted-foreground">{formatTokenDate(intl, token.created_at)}</TableCell>
                    <TableCell className="text-muted-foreground">{formatTokenLastUsed(intl, token.last_used_at)}</TableCell>
                    <TableCell>{t(revoked ? 'status.revoked' : 'status.active')}</TableCell>
                    <TableCell className="text-right">
                      {!revoked && <Button variant="destructive" size="sm" aria-label={`${t('revoke')} ${token.name}`} onClick={() => { setRevokeError(null); setRevoking(token); }}>{t('revoke')}</Button>}
                    </TableCell>
                  </TableRow>;
                })}
              </TableBody>
            </Table>
          </div>}
        <p className="border-t border-border px-4 py-3 text-xs text-muted-foreground">{t('limitFootnote', { max })}</p>
      </Card>

      <Card className="gap-3 p-4">
        <div>
          <h2 className="text-sm font-medium">{t('data.title')}</h2>
          <p className="text-sm text-muted-foreground">{t('data.desc')}</p>
        </div>
        <dl className="grid gap-3 sm:grid-cols-2">
          {DATA_ROWS.map(row => <div key={row} className="rounded-lg bg-muted/50 px-3 py-2">
            <dt className="text-sm font-medium">{t(`data.${row}`)}</dt>
            <dd className="text-xs text-muted-foreground">{t(`data.${row}.desc`)}</dd>
          </div>)}
        </dl>
        <p className="text-xs text-muted-foreground">{t('data.noContent')}</p>
      </Card>
    </>}

    {creating && <CreateDialog
      onClose={() => setCreating(false)}
      onStale={() => void load()}
      onCreated={result => { setCreating(false); setCreated(result); void load(); }}
    />}
    {created && <SecretDialog result={created} onClose={() => setCreated(null)} />}
    {revoking && <RevokeConfirmDialog
      title={t('revokeDialog.title', { name: revoking.name })}
      body={t('revokeDialog.body')}
      busy={revokeBusy}
      error={revokeError}
      onCancel={() => setRevoking(null)}
      onConfirm={() => void confirmRevoke()}
    />}
  </div>;
}

function CreateDialog({ onClose, onCreated, onStale }: {
  onClose: () => void;
  onCreated: (result: StatusTokenCreateResult) => void;
  onStale: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `externalStatus.${id}` });
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const trimmed = name.trim();
    if (!trimmed || busy) return;
    setBusy(true); setError(null);
    try {
      onCreated(await api.statusTokens.create(trimmed));
    } catch (err) {
      setError(statusTokenErrorMessage(intl, err));
      const code = statusTokenErrorCode(err);
      if (code === 'status_token_limit_reached' || code === 'external_status_disabled') onStale();
      setBusy(false);
    }
  };
  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose(); }}>
    <DialogContent className="sm:max-w-md">
      <form className="grid gap-4" onSubmit={event => void submit(event)}>
        <DialogHeader><DialogTitle>{t('createDialog.title')}</DialogTitle></DialogHeader>
        <div className="grid gap-1.5">
          <label htmlFor="status-token-name" className="text-sm font-medium">{t('createDialog.name')}</label>
          <Input id="status-token-name" value={name} maxLength={60} autoComplete="off" autoFocus onChange={event => setName(event.target.value)} />
          <span className="text-xs text-muted-foreground">{t('createDialog.hint')}</span>
        </div>
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={busy} onClick={onClose}>{t('createDialog.cancel')}</Button>
          <Button type="submit" disabled={busy || name.trim() === ''}>{t('createDialog.submit')}</Button>
        </DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}

/** Shown exactly once. Only the explicit button closes it, so the secret is not lost to a stray click. */
function SecretDialog({ result, onClose }: { result: StatusTokenCreateResult; onClose: () => void }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `externalStatus.${id}` }, values);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  const url = `${window.location.origin}${result.endpoint_path}`;
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(result.secret);
      setCopied(true); setCopyFailed(false);
    } catch { setCopied(false); setCopyFailed(true); }
  };
  return <Dialog open onOpenChange={() => { /* closed only via the explicit button */ }}>
    <DialogContent showClose={false} className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>{t('secret.title', { name: result.token.name })}</DialogTitle>
      </DialogHeader>
      <div role="alert" className="flex items-start gap-2 rounded-lg bg-warning/15 px-3 py-3 text-sm">
        <TriangleAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        {/* The visible warning doubles as the dialog description, so it is rendered once. */}
        <DialogDescription className="text-sm text-foreground"><b className="font-semibold">{t('secret.warningStrong')}</b>{t('secret.warning')}</DialogDescription>
      </div>
      <div className="grid gap-1.5">
        <label htmlFor="status-token-secret" className="text-sm font-medium">{t('secret.label')}</label>
        <div className="flex gap-2">
          <Input id="status-token-secret" readOnly value={result.secret} autoComplete="off" className="font-mono text-xs" onFocus={event => event.currentTarget.select()} />
          <Button className="shrink-0" onClick={() => void copy()}>{copied ? <Check /> : <Copy />}{t(copied ? 'secret.copied' : 'secret.copy')}</Button>
        </div>
        {copied && <span role="status" className="text-xs text-success">{t('secret.copiedNote')}</span>}
        {copyFailed && <span role="alert" className="text-xs text-destructive">{t('secret.copyFailed')}</span>}
      </div>
      <div className="grid gap-2 border-t border-border pt-3">
        <h3 className="text-sm font-medium">{t('secret.usage')}</h3>
        <pre className="overflow-x-auto whitespace-pre-wrap break-all rounded-lg bg-muted p-3 font-mono text-xs leading-relaxed">{`curl -H "Authorization: Bearer <${t('secret.label')}>" ${url}`}</pre>
        <h3 className="text-sm font-medium">{t('secret.returns')}</h3>
        <p className="text-xs text-muted-foreground">{t('secret.fields')}</p>
        <p className="text-xs text-muted-foreground">{t('data.noContent')}</p>
      </div>
      <DialogFooter>
        <Button onClick={onClose}>{t('secret.close')}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
