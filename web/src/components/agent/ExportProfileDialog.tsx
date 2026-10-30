import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type AgentProfileExport, type AgentProfileFormat } from '@/lib/api';
import { Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, useErrorMessage } from '@/components/mds';

const FORMATS: AgentProfileFormat[] = ['duduclaw', 'openclaw', 'hermes', 'agents-md', 'claude-subagent'];

function download(name: string, content: string) {
  const url = URL.createObjectURL(new Blob([content], { type: 'text/markdown;charset=utf-8' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  a.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Every returned file has passed the server's secret scan. Folder formats
 * expose individual downloads so the browser does not block a download burst. */
export function ExportProfileDialog({ agentId, onClose }: { agentId: string; onClose: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const errorText = useErrorMessage();
  const [format, setFormat] = useState<AgentProfileFormat>('duduclaw');
  const [includeUser, setIncludeUser] = useState(false);
  const [result, setResult] = useState<AgentProfileExport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const prepare = async () => {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      setResult(await api.agents.exportProfile(agentId, format, includeUser));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('agentProfile.export.title')}</DialogTitle>
          <DialogDescription>{t('agentProfile.export.description')}</DialogDescription>
        </DialogHeader>
        <label className="flex flex-col gap-2 text-sm">
          {t('agentProfile.export.format')}
          <select className="rounded-md border border-surface-border bg-background p-2" value={format} disabled={busy} onChange={(e) => { setFormat(e.target.value as AgentProfileFormat); setResult(null); }}>
            {FORMATS.map((value) => <option key={value} value={value}>{t(`agentProfile.export.format.${value}`)}</option>)}
          </select>
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={includeUser} disabled={busy} onChange={(e) => { setIncludeUser(e.target.checked); setResult(null); }} />
          {t('agentProfile.export.includeUser')}
        </label>
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        {result && (
          <div className="space-y-2" role="status">
            <p className="text-sm text-muted-foreground">{t(result.files.length > 1 ? 'agentProfile.export.folderHint' : 'agentProfile.export.ready')}</p>
            {result.files.map((file) => <Button key={file.name} variant="outline" onClick={() => download(file.name, file.content)}>{file.name}</Button>)}
          </div>
        )}
        <DialogFooter>
          <Button variant="ghost" onClick={onClose} disabled={busy}>{t('common.close')}</Button>
          <Button variant="brand" onClick={() => void prepare()} disabled={busy}>{t(busy ? 'agentProfile.export.preparing' : 'agentProfile.export.prepare')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
