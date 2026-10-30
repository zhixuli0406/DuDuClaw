import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Pencil, CircleOff } from 'lucide-react';
import { api, type ChatCredential, type CredentialAuditReport, type SecurityAuditVerification } from '@/lib/api';
import {
  Button, Card, CardContent, CardDescription, CardHeader, CardTitle,
  ErrorState, Input, Skeleton, Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/mds';

const PAGE_SIZE = 100;
const KINDS = ['password', 'api_key', 'oauth_token', 'ssh_key', 'certificate', 'connection_string', 'cookie', 'other'];
const STATUSES = ['active', 'revoked', 'rotated', 'expired', 'false_positive'];
const COLUMNS = ['kind', 'account', 'service', 'source', 'provider', 'holder', 'lastUsed', 'status'];

// Spreadsheet applications interpret formula prefixes even inside quoted CSV cells.
export function credentialCsvCell(value: string): string {
  const safe = /^[\s\uFEFF]*[=+\-@]/u.test(value) ? `'${value}` : value;
  return `"${safe.replaceAll('"', '""')}"`;
}

export function CredentialAuditPanel() {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `security.credentialAudit.${key}` });
  const [report, setReport] = useState<CredentialAuditReport | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [verification, setVerification] = useState<SecurityAuditVerification | null>(null);
  const [kind, setKind] = useState('');
  const [status, setStatus] = useState('');
  const [service, setService] = useState('');
  const [offset, setOffset] = useState(0);
  const [editing, setEditing] = useState<string | null>(null);
  const [serviceDraft, setServiceDraft] = useState('');
  const request = useRef(0);
  const query = useCallback(() => ({
    ...(kind ? { kind } : {}), ...(status ? { status } : {}),
    ...(service.trim() ? { service: service.trim() } : {}), limit: PAGE_SIZE, offset,
  }), [kind, status, service, offset]);

  const load = useCallback(async () => {
    const serial = ++request.current;
    setLoading(true);
    setError(null);
    try {
      const result = await api.credentials.auditList(query());
      if (serial === request.current) setReport(result);
    } catch (e) {
      if (serial === request.current) { setError(e); setReport(null); }
    } finally {
      if (serial === request.current) setLoading(false);
    }
  }, [query]);

  useEffect(() => {
    void load();
    return () => { request.current += 1; };
  }, [load]);

  async function update(params: { id: string; service?: string; status?: 'false_positive' }) {
    setBusy(true);
    setError(null);
    try {
      await api.credentials.auditUpdate(params);
      setEditing(null);
      await load();
    } catch (e) { setError(e); }
    finally { setBusy(false); }
  }

  async function verifyAudit() {
    setBusy(true);
    setError(null);
    setVerification(null);
    try { setVerification(await api.security.auditVerify()); }
    catch (e) { setError(e); }
    finally { setBusy(false); }
  }

  async function exportPage() {
    setBusy(true);
    setError(null);
    try {
      // The gateway records the export before releasing this bounded page.
      const result = await api.credentials.auditList({ ...query(), export: true });
      const rows = result.credentials.map((row) => [
        t(`kind.${row.kind}`), row.account_hint ?? '', row.service ?? '',
        `${t(`source.${row.source_kind}`)}: ${row.source_ref}`, row.provided_by,
        row.holders.filter((h) => !h.revoked_at).map((h) => `${h.holder_kind}:${h.holder_id}`).join('; '),
        row.last_used_at ?? '', t(`status.${row.status}`),
      ]);
      const csv = [COLUMNS.map((c) => t(c)), ...rows]
        .map((row) => row.map(credentialCsvCell).join(',')).join('\r\n');
      const url = URL.createObjectURL(new Blob(['\uFEFF', csv], { type: 'text/csv;charset=utf-8' }));
      const link = document.createElement('a');
      link.href = url;
      link.download = 'credential-audit.csv';
      document.body.appendChild(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) { setError(e); }
    finally { setBusy(false); }
  }

  function startEdit(row: ChatCredential) {
    setEditing(row.id);
    setServiceDraft(row.service ?? '');
  }

  function providerLabel(row: ChatCredential) {
    if (!row.provided_by) return '—';
    if (/^webchat-user:[a-f0-9]+$/i.test(row.provided_by) || row.provided_by === 'webchat-anonymous') {
      return t('provider.webchatGuest');
    }
    if (/^[a-f0-9]{64}$/i.test(row.provided_by)) {
      return t('provider.chat');
    }
    return row.provided_by;
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('title')}</CardTitle>
        <CardDescription>{t('description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {report && <p role="note" className="rounded-lg border border-surface-border p-3 text-sm">
          {t(report.mode === 'audit' ? 'auditNotice' : 'maskNotice')}
        </p>}
        <div className="flex flex-wrap items-end gap-3">
          <label className="space-y-1 text-sm">
            <span className="block">{t('kind')}</span>
            <select className="h-9 rounded border border-surface-border bg-background px-2" value={kind}
              disabled={busy} onChange={(e) => { setKind(e.target.value); setOffset(0); }}>
              <option value="">{t('all')}</option>
              {KINDS.map((k) => <option key={k} value={k}>{t(`kind.${k}`)}</option>)}
            </select>
          </label>
          <label className="space-y-1 text-sm">
            <span className="block">{t('status')}</span>
            <select className="h-9 rounded border border-surface-border bg-background px-2" value={status}
              disabled={busy} onChange={(e) => { setStatus(e.target.value); setOffset(0); }}>
              <option value="">{t('all')}</option>
              {STATUSES.map((s) => <option key={s} value={s}>{t(`status.${s}`)}</option>)}
            </select>
          </label>
          <label className="space-y-1 text-sm">
            <span className="block">{t('service')}</span>
            <Input value={service} maxLength={160} disabled={busy}
              onChange={(e) => { setService(e.target.value); setOffset(0); }} />
          </label>
          <Button variant="outline" disabled={loading || busy} onClick={() => void load()}>{t('refresh')}</Button>
          <Button variant="outline" disabled={busy} onClick={() => void verifyAudit()}>{t('verify')}</Button>
          <Button variant="outline" disabled={loading || busy || !report?.credentials.length}
            onClick={() => void exportPage()}>{t('export')}</Button>
        </div>
        <p className="text-xs text-muted-foreground">{t('verifyScope')}</p>
        {verification && <p role="status" className="text-sm">
          {t(verification.scope === 'current_segment' && verification.status === 'current_segment_consistent'
            && verification.intact && (verification.lines_checked ?? 0) > 0 ? 'verifyConsistent'
            : verification.status === 'broken' ? 'verifyBroken' : 'verifyUnverified')}
        </p>}
        {error != null && <ErrorState variant="inline" error={error} title={t('error')} onRetry={() => void load()} />}
        {loading ? <Skeleton className="h-32 w-full" /> : report && (
          <>
            <div className="min-w-0 max-w-full">
              <Table className="min-w-2xl table-fixed [&_td]:whitespace-normal [&_td]:break-words [&_th]:whitespace-normal">
                <TableHeader><TableRow>
                  {COLUMNS.map((c) => <TableHead key={c} className={c === 'source' ? 'w-1/6' : undefined}>{t(c)}</TableHead>)}
                  <TableHead>{t('actions')}</TableHead>
                </TableRow></TableHeader>
                <TableBody>
                  {report.credentials.map((row) => <TableRow key={row.id}>
                    <TableCell>{t(`kind.${row.kind}`)}</TableCell>
                    <TableCell><span className="block truncate" title={row.account_hint ?? undefined}>{row.account_hint ?? '—'}</span></TableCell>
                    <TableCell><span className="block truncate" title={row.service ?? undefined}>{row.service ?? t('unknown')}</span></TableCell>
                    <TableCell><span>{t(`source.${row.source_kind}`)}</span><small className="block break-all text-muted-foreground" title={row.source_ref}>{row.source_ref}</small></TableCell>
                    <TableCell><span className="block truncate" title={row.provided_by}>{providerLabel(row)}</span></TableCell>
                    <TableCell>{row.holders.some((h) => !h.revoked_at) ? row.holders.filter((h) => !h.revoked_at).map((h) => (
                      <span key={`${h.holder_kind}:${h.holder_id}`} className="block truncate" title={`${t(`holder.${h.holder_kind}`)}: ${h.holder_id}`}>
                        {t(`holder.${h.holder_kind}`)}: {h.holder_id.length > 24 ? `${h.holder_id.slice(0, 12)}…${h.holder_id.slice(-6)}` : h.holder_id}
                      </span>
                    )) : '—'}</TableCell>
                    <TableCell>{row.last_used_at && Number.isFinite(Date.parse(row.last_used_at)) ? intl.formatDate(row.last_used_at, { dateStyle: 'medium', timeStyle: 'short' }) : t('notObserved')}</TableCell>
                    <TableCell>{t(`status.${row.status}`)}</TableCell>
                    <TableCell>
                      <div className="flex gap-1">
                        <Button size="icon-xs" variant="ghost" aria-label={t('editService')} title={t('editService')} disabled={busy} onClick={() => startEdit(row)}><Pencil /></Button>
                        <Button size="icon-xs" variant="ghost" aria-label={t('falsePositive')} title={t('falsePositive')} disabled={busy || row.status === 'false_positive'}
                          onClick={() => void update({ id: row.id, status: 'false_positive' })}><CircleOff /></Button>
                      </div>
                    </TableCell>
                  </TableRow>)}
                  {report.credentials.length === 0 && <TableRow><TableCell colSpan={9} className="py-6 text-center">{t('empty')}</TableCell></TableRow>}
                </TableBody>
              </Table>
            </div>
            {editing && <form className="flex flex-wrap items-end gap-2" onSubmit={(e) => {
              e.preventDefault(); void update({ id: editing, service: serviceDraft.trim() });
            }}>
              <label className="space-y-1 text-sm"><span className="block">{t('newService')}</span>
                <Input value={serviceDraft} maxLength={160} required disabled={busy} onChange={(e) => setServiceDraft(e.target.value)} />
              </label>
              <Button type="submit" disabled={busy || !serviceDraft.trim()}>{t('save')}</Button>
              <Button type="button" variant="ghost" disabled={busy} onClick={() => setEditing(null)}>{t('cancel')}</Button>
            </form>}
            <div className="flex items-center justify-end gap-2">
              <span className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'security.credentialAudit.page' }, { page: offset / PAGE_SIZE + 1 })}</span>
              <Button variant="outline" disabled={busy || offset === 0} onClick={() => setOffset((n) => Math.max(0, n - PAGE_SIZE))}>{t('previous')}</Button>
              <Button variant="outline" disabled={busy || (report.total != null ? offset + report.credentials.length >= report.total : report.credentials.length < PAGE_SIZE)} onClick={() => setOffset((n) => n + PAGE_SIZE)}>{t('next')}</Button>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}
