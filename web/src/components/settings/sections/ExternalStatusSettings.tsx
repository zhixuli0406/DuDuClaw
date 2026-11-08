import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { TriangleAlert } from 'lucide-react';
import { api, type StatusTokenAdminEntry } from '@/lib/api';
import {
  Button, Input, Switch, Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/mds';
import { toast } from '@/lib/toast';
import {
  RevokeConfirmDialog, formatTokenDate, formatTokenLastUsed, statusTokenErrorCode, statusTokenErrorMessage,
} from '@/components/external-status/shared';

const RISKS = ['scope', 'leak', 'limit', 'off'] as const;

function ownerLabel(entry: StatusTokenAdminEntry, fallback: string): string {
  return entry.user_display_name?.trim() || entry.user_email || fallback;
}

/** Settings → Security & records → External system queries (RM-064). Admin only. */
export function ExternalStatusSettings() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `externalStatus.settings.${id}` }, values);
  const tc = (id: string) => intl.formatMessage({ id: `externalStatus.${id}` });
  const [enabled, setEnabled] = useState(false);
  const [switchReady, setSwitchReady] = useState(false);
  const [saving, setSaving] = useState(false);
  const [tokens, setTokens] = useState<StatusTokenAdminEntry[] | null>(null);
  const [listFailed, setListFailed] = useState(false);
  const [query, setQuery] = useState('');
  const [revoking, setRevoking] = useState<StatusTokenAdminEntry | null>(null);
  const [revokeBusy, setRevokeBusy] = useState(false);
  const [revokeError, setRevokeError] = useState<string | null>(null);
  const mounted = useRef(true);

  const loadList = useCallback(async () => {
    try {
      const result = await api.statusTokens.adminList();
      if (!mounted.current) return;
      setTokens(result.tokens); setListFailed(false);
    } catch {
      if (mounted.current) setListFailed(true);
    }
  }, []);
  useEffect(() => {
    mounted.current = true;
    void api.system.config().then(config => {
      if (!mounted.current) return;
      setEnabled(config.external_status_enabled ?? false);
      setSwitchReady(true);
    }).catch(() => { if (mounted.current) setSwitchReady(true); });
    void loadList();
    return () => { mounted.current = false; };
  }, [loadList]);

  const toggle = async (next: boolean) => {
    setSaving(true); setEnabled(next);
    try {
      await api.system.updateConfig({ external_status_enabled: next });
    } catch {
      if (mounted.current) setEnabled(!next);
      toast.error(t('saveFailed'));
    } finally { if (mounted.current) setSaving(false); }
  };

  const confirmRevoke = async () => {
    if (!revoking) return;
    setRevokeBusy(true); setRevokeError(null);
    try {
      await api.statusTokens.adminRevoke(revoking.id);
      setRevoking(null);
      toast.success(tc('revoked'));
      await loadList();
    } catch (err) {
      setRevokeError(statusTokenErrorMessage(intl, err));
      if (statusTokenErrorCode(err) === 'status_token_not_found') void loadList();
    } finally { if (mounted.current) setRevokeBusy(false); }
  };

  const all = tokens ?? [];
  const needle = query.trim().normalize('NFKC').toLocaleLowerCase();
  const rows = needle === '' ? all : all.filter(entry =>
    [entry.name, entry.user_display_name, entry.user_email].some(value => value?.normalize('NFKC').toLocaleLowerCase().includes(needle)));
  const activeCount = all.filter(entry => !entry.revoked_at && entry.active).length;
  const former = t('formerUser');

  return <div className="space-y-4">
    <div className="flex items-start justify-between gap-4">
      <div className="space-y-1">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">{t('title')}</span>
          <span className="rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">{t('badge')}</span>
        </div>
        <p className="text-sm text-muted-foreground">{t('desc')}</p>
      </div>
      <Switch checked={enabled} disabled={!switchReady || saving} aria-label={t('switch')} onCheckedChange={next => void toggle(next)} />
    </div>

    <div className="flex gap-3 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm">
      <TriangleAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <div>
        <p className="mb-1.5 font-medium">{t('risk.title')}</p>
        <ul className="list-disc space-y-1.5 pl-5">
          {RISKS.map(risk => <li key={risk}>{t(`risk.${risk}`)}</li>)}
        </ul>
      </div>
    </div>

    <section aria-label={t('all')} className="overflow-hidden rounded-lg border border-border">
      <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
        <div>
          <h3 className="text-sm font-medium">{t('all')}</h3>
          {tokens && <p className="text-xs text-muted-foreground">{t('summary', { total: all.length, active: activeCount })}</p>}
        </div>
        <Input type="search" className="w-60" aria-label={t('search')} placeholder={t('search')} value={query} onChange={event => setQuery(event.target.value)} />
      </div>
      {listFailed && <p role="alert" className="border-t border-border px-4 py-3 text-sm text-destructive">{t('loadFailed')}</p>}
      {tokens && rows.length === 0 && <p className="border-t border-border px-4 py-6 text-center text-sm text-muted-foreground">{t(all.length === 0 ? 'empty' : 'noMatch')}</p>}
      {rows.length > 0 && <div className="overflow-x-auto border-t border-border">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{tc('col.user')}</TableHead>
              <TableHead>{tc('col.name')}</TableHead>
              <TableHead>{tc('col.created')}</TableHead>
              <TableHead>{tc('col.lastUsed')}</TableHead>
              <TableHead>{tc('col.status')}</TableHead>
              <TableHead><span className="sr-only">{tc('revoke')}</span></TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map(entry => {
              const revoked = Boolean(entry.revoked_at);
              const owner = ownerLabel(entry, former);
              return <TableRow key={entry.id}>
                <TableCell>{owner}</TableCell>
                <TableCell className={revoked ? 'text-muted-foreground' : undefined}>{entry.name}</TableCell>
                <TableCell className="text-muted-foreground">{formatTokenDate(intl, entry.created_at)}</TableCell>
                <TableCell className="text-muted-foreground">{formatTokenLastUsed(intl, entry.last_used_at)}</TableCell>
                <TableCell>{tc(revoked ? 'status.revoked' : 'status.active')}</TableCell>
                <TableCell className="text-right">
                  {!revoked && <Button variant="destructive" size="sm" aria-label={`${tc('revoke')} ${owner} ${entry.name}`} onClick={() => { setRevokeError(null); setRevoking(entry); }}>{tc('revoke')}</Button>}
                </TableCell>
              </TableRow>;
            })}
          </TableBody>
        </Table>
      </div>}
    </section>

    {revoking && <RevokeConfirmDialog
      title={intl.formatMessage({ id: 'externalStatus.adminRevokeDialog.title' }, { user: ownerLabel(revoking, former), name: revoking.name })}
      body={intl.formatMessage({ id: 'externalStatus.adminRevokeDialog.body' }, { user: ownerLabel(revoking, former) })}
      busy={revokeBusy}
      error={revokeError}
      onCancel={() => setRevoking(null)}
      onConfirm={() => void confirmRevoke()}
    />}
  </div>;
}
