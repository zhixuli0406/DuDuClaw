import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Download } from 'lucide-react';
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, Input } from '@/components/mds';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { complianceDateRange, complianceLanguage, complianceReports, downloadComplianceReport, type ComplianceFormat } from '@/lib/compliance-report';

export function ComplianceReportButton() {
  const role = useAuthStore((s) => s.user?.role);
  const edition = useSystemStore((s) => s.status?.edition_profile);
  return role === 'admin' && edition === 'enterprise' ? <ComplianceReportDialog /> : null;
}
function ComplianceReportDialog() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `compliance.${id}` });
  const [open, setOpen] = useState(false);
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [format, setFormat] = useState<ComplianceFormat>('xlsx');
  const [personal, setPersonal] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const sequence = useRef(0);
  useEffect(() => () => { sequence.current++; }, []);
  const generate = async () => {
    const run = ++sequence.current;
    setBusy(true); setError(false);
    try {
      const report = await complianceReports.generate({ ...complianceDateRange(start, end), language: complianceLanguage(intl.locale), format, include_personal_data: personal });
      if (run === sequence.current) downloadComplianceReport(report, format);
    } catch { if (run === sequence.current) setError(true); }
    finally { if (run === sequence.current) setBusy(false); }
  };
  const changeOpen = (value: boolean) => {
    setOpen(value);
    if (!value) { sequence.current++; setBusy(false); setPersonal(false); setError(false); }
  };
  return <>
    <Button variant="outline" size="sm" onClick={() => changeOpen(true)}><Download className="size-4" />{t('open')}</Button>
    <Dialog open={open} onOpenChange={changeOpen}><DialogContent className="sm:max-w-lg"><DialogHeader><DialogTitle>{t('title')}</DialogTitle></DialogHeader>
      <div className="space-y-4">
        <p className="text-sm text-muted-foreground">{t('description')}</p>
        <div className="grid grid-cols-2 gap-3">
          <label className="grid gap-1 text-sm">{t('from')}<Input type="date" value={start} disabled={busy} onChange={(e) => setStart(e.target.value)} /></label>
          <label className="grid gap-1 text-sm">{t('through')}<Input type="date" value={end} disabled={busy} onChange={(e) => setEnd(e.target.value)} /></label>
        </div>
        <p className="text-xs text-muted-foreground">{t('rangeNote')}</p>
        <label className="grid gap-1 text-sm">{t('format')}<select className="rounded border border-surface-border bg-background p-2" value={format} disabled={busy} onChange={(e) => setFormat(e.target.value as ComplianceFormat)}>
          <option value="csv">CSV (UTF-8)</option><option value="xlsx">Excel (.xlsx)</option><option value="html">{t('html')}</option>
        </select></label>
        <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={personal} disabled={busy} onChange={(e) => setPersonal(e.target.checked)} className="mt-1" /><span>{t('personal')}</span></label>
        <p className="text-xs text-muted-foreground">{t('maskNote')}</p>
        {error && <p role="alert" className="text-sm text-destructive">{t('error')}</p>}
        <div className="flex justify-end"><Button disabled={busy || !start || !end} onClick={() => void generate()}>{t(busy ? 'generating' : 'generate')}</Button></div>
      </div>
    </DialogContent></Dialog>
  </>;
}
