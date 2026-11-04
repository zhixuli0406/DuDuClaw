import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { FileAudio, Download, RefreshCw, X } from 'lucide-react';
import { Button } from '@/components/mds';
import { api, type CallableAgent } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { meetingApi, uploadMeeting, type MeetingJob } from '@/lib/meeting/api';
import { prepareMeetingAudio } from '@/lib/meeting/audio';

export function MeetingTranscriptsPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const user = useAuthStore(s => s.user);
  const [agents, setAgents] = useState<CallableAgent[]>([]);
  const [agent, setAgent] = useState('');
  const [file, setFile] = useState<File | null>(null);
  const [maxBytes, setMaxBytes] = useState(4 * 1024 ** 3);
  const [jobs, setJobs] = useState<MeetingJob[]>([]);
  const [error, setError] = useState('');
  const [phase, setPhase] = useState<'idle' | 'preparing' | 'uploading'>('idle');
  const [path, setPath] = useState<'audio' | 'original' | null>(null);
  const [progress, setProgress] = useState(0);
  const [revision, setRevision] = useState(0);
  const controller = useRef<AbortController | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);
  const active = phase !== 'idle';
  const showError = (e: unknown) => setError(e instanceof Error ? (e.message.startsWith('meeting.') ? t(e.message) : e.message) : t('meeting.requestFailed'));
  useEffect(() => {
    let alive = true;
    setAgents([]); setAgent(''); setJobs([]);
    api.agents.callable().then(result => {
      if (!alive) return;
      const allowed = result.agents.filter(item => user?.role === 'admin' || (item.via === 'binding' && item.selectable && (item.level === 'operator' || item.level === 'owner') && item.role !== 'main' && item.name !== result.default_agent));
      setAgents(allowed); setAgent(allowed[0]?.name ?? '');
    }).catch(e => { if (alive) showError(e); });
    return () => { alive = false; controller.current?.abort(); };
  }, [user?.id, user?.role]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    setJobs([]);
    if (!agent) return;
    let alive = true;
    let pending = false;
    const refresh = async () => {
      if (pending) return;
      pending = true;
      try { const result = await meetingApi.list(agent); if (alive) { setJobs(result.jobs); setMaxBytes(result.max_bytes); } }
      catch (e) { if (alive) showError(e); }
      finally { pending = false; }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => { alive = false; window.clearInterval(timer); };
  }, [agent, revision]); // eslint-disable-line react-hooks/exhaustive-deps
  async function submit() {
    if (!file || !agent || active || !user) return;
    if (file.size === 0 || file.size > maxBytes) { setError(t('meeting.sizeError')); return; }
    setError(''); setProgress(0); setPath(null); setPhase('preparing');
    const abort = new AbortController(); controller.current = abort;
    try {
      const prepared = await prepareMeetingAudio(file, abort.signal);
      abort.signal.throwIfAborted();
      setPath(prepared.path); setPhase('uploading');
      await uploadMeeting(prepared.file, agent, user.id, setProgress, abort.signal);
      setFile(null); if (fileInput.current) fileInput.current.value = ''; setRevision(value => value + 1);
    } catch (e) { if (!abort.signal.aborted) showError(e); }
    finally { setPhase('idle'); controller.current = null; }
  }
  return <main className="mx-auto w-full max-w-5xl space-y-8 p-5 md:p-8">
    <header className="space-y-2">
      <h1 className="text-2xl font-semibold tracking-tight">{t('meeting.title')}</h1>
      <p className="max-w-2xl text-sm leading-6 text-muted-foreground">{t('meeting.description')}</p>
    </header>
    {error && <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error}</p>}
    <section aria-label={t('meeting.upload')} className="space-y-5 rounded-xl border border-border bg-card p-5 md:p-6">
      <div className="max-w-sm space-y-2">
        <label htmlFor="meeting-agent" className="block text-sm font-medium">{t('meeting.employee')}</label>
        <select id="meeting-agent" value={agent} disabled={active || agents.length === 0} onChange={e => { setAgent(e.target.value); setError(''); }}
          className="h-10 w-full rounded-lg border border-input bg-background px-3 text-sm focus-visible:outline-2 focus-visible:outline-ring">
          {agents.length === 0 && <option value="">{t('meeting.noEmployees')}</option>}
          {agents.map(item => <option key={item.name} value={item.name}>{item.display_name || item.name}</option>)}
        </select>
      </div>
      <div className="space-y-3 border-y border-border py-5">
        <label htmlFor="meeting-file" className="flex items-center gap-2 text-sm font-medium"><FileAudio aria-hidden="true" className="size-4" />{t('meeting.chooseFile')}</label>
        <input ref={fileInput} id="meeting-file" type="file" accept="audio/*,video/*,.m4a,.mkv,.mov,.aac,.opus" disabled={active || !agent}
          onChange={e => { setFile(e.target.files?.[0] ?? null); setPath(null); setError(''); }}
          className="block w-full text-sm file:mr-4 file:rounded-lg file:border file:border-border file:bg-muted file:px-3 file:py-2 file:text-foreground" />
        <p className="text-xs leading-5 text-muted-foreground">{t('meeting.limit', { size: (maxBytes / 1024 ** 3).toFixed(1) })}</p>
        {path && <p role="status" className="text-sm text-muted-foreground">{t(`meeting.path.${path}`)}</p>}
      </div>
      {active && <div className="space-y-2" aria-live="polite">
        <p className="text-sm">{t(phase === 'preparing' ? 'meeting.preparingAudio' : 'meeting.uploadProgress', { progress: Math.round(progress * 100) })}</p>
        {phase === 'uploading' && <progress aria-label={t('meeting.uploadProgressLabel')} value={progress} max={1} className="h-2 w-full accent-primary" />}
      </div>}
      <div className="flex flex-wrap items-center gap-3">
        <Button size="lg" disabled={!agent || !file || active} onClick={() => void submit()}>{t('meeting.start')}</Button>
        {active && <Button variant="ghost" onClick={() => controller.current?.abort()}>{t('meeting.pauseUpload')}</Button>}
        <span className="text-xs text-muted-foreground">{t('meeting.resumeHint')}</span>
      </div>
    </section>
    <section aria-labelledby="meeting-jobs-title" className="space-y-3">
      <div className="flex items-center justify-between gap-3">
        <h2 id="meeting-jobs-title" className="text-lg font-medium">{t('meeting.jobs')}</h2>
        <Button variant="ghost" size="sm" disabled={!agent} onClick={() => setRevision(value => value + 1)}><RefreshCw aria-hidden="true" />{t('meeting.refresh')}</Button>
      </div>
      <MeetingJobList jobs={jobs} onCancel={async id => { try { await meetingApi.cancel(agent, id); setRevision(value => value + 1); } catch (e) { showError(e); } }}
        onDownload={async (id, format) => { try { await meetingApi.download(agent, id, format); } catch (e) { showError(e); } }} />
    </section>
  </main>;
}
export function MeetingJobList({ jobs, onCancel, onDownload }: { jobs: MeetingJob[]; onCancel: (id: string) => void; onDownload: (id: string, format: 'md' | 'json') => void }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  if (jobs.length === 0) return <p className="py-8 text-sm text-muted-foreground">{t('meeting.empty')}</p>;
  return <ul className="divide-y divide-border rounded-xl border border-border bg-card px-4 md:px-5">
    {jobs.map(job => <li key={job.job_id} className="flex flex-wrap items-center justify-between gap-4 py-4">
      <div className="min-w-0 flex-1 space-y-1">
        <p className="text-sm font-medium">{t(`meeting.state.${job.state}`, { chunk: job.chunk, total: job.chunks })}</p>
        <p className="text-xs text-muted-foreground">{intl.formatDate(job.created_at * 1000, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}</p>
        {job.state === 'transcribing' && job.chunks > 0 && <progress aria-label={t('meeting.jobProgress')} value={Math.max(0, job.chunk - 1)} max={job.chunks} className="h-1.5 w-full max-w-xs accent-primary" />}
        {job.error && <p className="text-sm text-destructive">{job.error}</p>}
      </div>
      <div className="flex flex-wrap gap-2">
        {job.state === 'done' && <><Button variant="outline" size="sm" onClick={() => onDownload(job.job_id, 'md')}><Download aria-hidden="true" />{t('meeting.downloadTranscript')}</Button><Button variant="ghost" size="sm" onClick={() => onDownload(job.job_id, 'json')}>JSON</Button></>}
        {['queued', 'preparing', 'transcribing'].includes(job.state) && <Button variant="ghost" size="sm" onClick={() => onCancel(job.job_id)}><X aria-hidden="true" />{t('meeting.cancel')}</Button>}
      </div>
    </li>)}
  </ul>;
}
