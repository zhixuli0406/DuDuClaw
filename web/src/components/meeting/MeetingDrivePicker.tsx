import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Button } from '@/components/mds';
import { meetingDriveApi, type DriveRecordingFile, type MeetRecording, type MeetingDriveStatus, type PickedRecording } from '@/lib/meeting/drive';
import { pickGoogleRecording, prepareGooglePicker } from '@/lib/meeting/picker';

export function MeetingDrivePicker({ agent, agentName, disabled, onBusy, onImported }: {
  agent: string; agentName: string; disabled: boolean; onBusy: (busy: boolean) => void; onImported: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<MeetingDriveStatus | null>(null);
  const [files, setFiles] = useState<DriveRecordingFile[]>([]);
  const [recordings, setRecordings] = useState<MeetRecording[]>([]);
  const [filesNext, setFilesNext] = useState<string>();
  const [recordingsNext, setRecordingsNext] = useState<string>();
  const [truncated, setTruncated] = useState(false);
  const [search, setSearch] = useState('');
  const [appliedSearch, setAppliedSearch] = useState('');
  const [selected, setSelected] = useState<PickedRecording | null>(null);
  const [busy, setBusy] = useState(false);
  const [importing, setImporting] = useState(false);
  const [pickerReady, setPickerReady] = useState(false);
  const [error, setError] = useState('');
  const controller = useRef<AbortController | null>(null);
  const alive = useRef(true);
  const busyRef = useRef(false);
  useEffect(() => { alive.current = true; return () => { alive.current = false; controller.current?.abort(); }; }, []);
  const showError = (e: unknown) => setError(e instanceof Error ? (e.message.startsWith('meeting.') ? t(e.message) : e.message) : t('meeting.requestFailed'));
  async function run(action: (signal: AbortSignal) => Promise<void>) {
    if (busyRef.current || disabled || !agent) return;
    busyRef.current = true; setBusy(true); onBusy(true); setError('');
    const abort = new AbortController(); controller.current = abort;
    try { await action(abort.signal); }
    catch (e) { if (alive.current && !abort.signal.aborted) showError(e); }
    finally {
      if (alive.current) { busyRef.current = false; setBusy(false); setImporting(false); onBusy(false); controller.current = null; }
    }
  }
  async function load() {
    setOpen(true); setStatus(null); setSelected(null); setFiles([]); setRecordings([]); setFilesNext(undefined); setRecordingsNext(undefined); setSearch(''); setAppliedSearch(''); setTruncated(false); setPickerReady(false);
    await run(async signal => {
      const current = await meetingDriveApi.status(signal);
      if (signal.aborted) return;
      setStatus(current);
      if (!current.authorized) return;
      // SDK setup is independent of the lists; a Meet API error must not hide shared Drive files.
      if (current.picker_configured) void prepareGooglePicker().then(() => {
        if (alive.current && !signal.aborted) setPickerReady(true);
      }).catch(e => { if (alive.current && !signal.aborted) showError(e); });
      // The gateway admits one Google request flow per person at a time.
      try {
        const drive = await meetingDriveApi.files(agent, '', undefined, signal);
        if (signal.aborted) return;
        setFiles(drive.files); setFilesNext(drive.next_page_token);
      } catch (e) { if (!signal.aborted) showError(e); }
      if (signal.aborted) return;
      try {
        const meet = await meetingDriveApi.recordings(agent, undefined, signal);
        if (signal.aborted) return;
        setRecordings(meet.recordings); setRecordingsNext(meet.next_page_token); setTruncated(meet.truncated);
      } catch (e) { if (!signal.aborted) showError(e); }
    });
  }
  async function pick(fileId?: string) {
    if (!status || !pickerReady) return;
    await run(async signal => {
      setSelected(null);
      const picked = await pickGoogleRecording(status, intl.locale, signal, fileId);
      if (!picked || signal.aborted) return;
      setSelected(picked);
    });
  }
  const blocked = busy || disabled || !agent;
  return <div className="space-y-4 border-t border-border pt-5">
    <Button variant="outline" disabled={blocked} onClick={() => void load()}>{t('meeting.drive.choose')}</Button>
    {open && <div className="space-y-4">
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {status && !status.authorized && <p className="text-sm leading-6">{t('meeting.drive.consentNeeded')} <Link className="underline" to="/me/connections">{t('myConnections.title')}</Link></p>}
      {status?.authorized && <>
        <p className="text-sm text-muted-foreground">{t('meeting.drive.account', { email: status.account_email || '' })}</p>
        {!status.picker_configured && <p className="text-sm text-warning">{t('meeting.drive.setupNeeded')}</p>}
        <Button variant="outline" disabled={blocked || !pickerReady} onClick={() => void pick()}>{t('meeting.drive.browse')}</Button>
        <section aria-labelledby="meet-recordings-title" className="space-y-2">
          <h3 id="meet-recordings-title" className="text-sm font-medium">{t('meeting.drive.recent')}</h3>
          <p className="text-xs text-muted-foreground">{t('meeting.drive.recentHint')}</p>
          {recordings.length === 0 && <p className="text-sm text-muted-foreground">{t('meeting.drive.noRecordings')}</p>}
          <ul className="max-h-64 space-y-2 overflow-y-auto">
            {recordings.map(row => <li key={`${row.conference}:${row.file_id}`}>
              <Button variant="ghost" disabled={blocked || !pickerReady} onClick={() => void pick(row.file_id)}>
                {t('meeting.drive.recordingAt', { time: Number.isNaN(Date.parse(row.start_time)) ? row.start_time : intl.formatDate(row.start_time, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) })}
              </Button>
            </li>)}
          </ul>
          {truncated && <p className="text-xs text-muted-foreground">{t('meeting.drive.moreHint')}</p>}
          {recordingsNext && <Button size="sm" variant="ghost" disabled={blocked} onClick={() => void run(async signal => {
            const next = await meetingDriveApi.recordings(agent, recordingsNext, signal);
            if (!signal.aborted) { setRecordings(rows => [...rows, ...next.recordings].filter((row, index, all) => all.findIndex(item => item.file_id === row.file_id) === index)); setRecordingsNext(next.next_page_token); setTruncated(next.truncated); }
          })}>{t('meeting.drive.more')}</Button>}
        </section>
        <section aria-labelledby="drive-files-title" className="space-y-3">
          <h3 id="drive-files-title" className="text-sm font-medium">{t('meeting.drive.sharedFiles')}</h3>
          <p className="text-xs text-muted-foreground">{t('meeting.drive.sharedHint')}</p>
          <form className="flex flex-wrap gap-2" onSubmit={e => { e.preventDefault(); void run(async signal => {
            const result = await meetingDriveApi.files(agent, search, undefined, signal);
            if (!signal.aborted) { setFiles(result.files); setFilesNext(result.next_page_token); setAppliedSearch(search); }
          }); }}>
            <label className="sr-only" htmlFor="meeting-drive-search">{t('meeting.drive.searchLabel')}</label>
            <input id="meeting-drive-search" value={search} maxLength={200} disabled={blocked} onChange={e => setSearch(e.target.value)} className="h-10 min-w-0 flex-1 rounded-lg border border-input bg-background px-3 text-sm" />
            <Button type="submit" variant="outline" disabled={blocked}>{t('meeting.drive.search')}</Button>
          </form>
          {files.length === 0 && <p className="text-sm text-muted-foreground">{t('meeting.drive.noFiles')}</p>}
          <ul className="max-h-64 space-y-2 overflow-y-auto">
            {files.map(row => <li key={row.id}><Button variant="ghost" className="max-w-full" disabled={blocked || !pickerReady} onClick={() => void pick(row.id)}><span className="truncate">{row.name}</span><span className="text-xs text-muted-foreground">{(row.size / 1024 ** 2).toFixed(1)} MB</span></Button></li>)}
          </ul>
          {filesNext && <Button size="sm" variant="ghost" disabled={blocked} onClick={() => void run(async signal => {
            const next = await meetingDriveApi.files(agent, appliedSearch, filesNext, signal);
            if (!signal.aborted) { setFiles(rows => [...rows, ...next.files].filter((row, index, all) => all.findIndex(item => item.id === row.id) === index)); setFilesNext(next.next_page_token); }
          })}>{t('meeting.drive.more')}</Button>}
        </section>
        {selected && <section aria-label={t('meeting.drive.confirmTitle')} className="space-y-3 rounded-lg border border-border bg-muted/30 p-4">
          <h3 className="break-words text-sm font-medium">{selected.name}</h3>
          <p className="text-sm leading-6">{t('meeting.drive.confirmHint', { employee: agentName })}</p>
          <div className="flex gap-2">
            <Button disabled={blocked} onClick={() => void run(async signal => {
              setImporting(true);
              // Register this exact file and employee only after explicit confirmation.
              // A failed import consumes its receipt; another attempt needs confirmation again.
              setSelected(null);
              const registered = await meetingDriveApi.registerSelection(agent, selected.id, signal);
              if (signal.aborted) return;
              await meetingDriveApi.import(agent, selected.id, registered.selection_receipt, signal);
              if (!signal.aborted) onImported();
            })}>{t('meeting.drive.confirm')}</Button>
            <Button variant="ghost" disabled={blocked} onClick={() => setSelected(null)}>{t('meeting.drive.clear')}</Button>
          </div>
        </section>}
      </>}
      {busy && <p role="status" className="text-sm text-muted-foreground">{t(importing ? 'meeting.drive.importing' : 'meeting.drive.loading')}</p>}
    </div>}
  </div>;
}
