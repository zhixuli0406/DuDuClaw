import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Eye, Square } from 'lucide-react';
import { Button } from '@/components/mds';
import { client } from '@/lib/ws-client';
import { ComputerSessionPanel } from './ComputerSessionPanel';

interface TeamStatus {
  active: boolean;
  session_id?: string;
  state?: 'running' | 'paused';
}
interface Snapshot {
  src: string;
  capturedAt: number;
  stale: boolean;
  fullyMasked: boolean;
}

function snapshot(value: unknown, expected: string): Snapshot | null {
  if (!value || typeof value !== 'object') return null;
  const v = value as Record<string, unknown>;
  if (v.ok !== true || v.session_id !== expected || typeof v.png_base64 !== 'string'
    || v.png_base64.length > 28_000_000 || !/^[A-Za-z0-9+/]+={0,2}$/.test(v.png_base64)
    || typeof v.captured_at !== 'number' || !Number.isFinite(v.captured_at) || Math.abs(v.captured_at) > 8.64e15
    || typeof v.stale !== 'boolean' || typeof v.fully_masked !== 'boolean') return null;
  try {
    if (atob(v.png_base64.slice(0, 12)).slice(0, 8) !== '\x89PNG\r\n\x1a\n') return null;
  } catch { return null; }
  return { src: `data:image/png;base64,${v.png_base64}`, capturedAt: v.captured_at, stale: v.stale, fullyMasked: v.fully_masked };
}

/** The standalone page reads only computer-session RPCs. The server, not
 * the route or its link token, selects direct access versus team viewing. */
export function ComputerWatchPanel({ agentId, expectedSession, onExpired }: {
  agentId: string;
  expectedSession?: string;
  onExpired?: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `computerWatch.${id}` });
  const [mode, setMode] = useState<'loading' | 'direct' | 'masked' | 'denied'>('loading');
  const [status, setStatus] = useState<TeamStatus | null>(null);
  const [image, setImage] = useState<Snapshot | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const epoch = useRef(0);
  const refreshRef = useRef<() => Promise<void>>(async () => {});

  useEffect(() => {
    let alive = true;
    let checking = false;
    let previousSession: string | null = null;
    const current = ++epoch.current;
    const valid = () => alive && epoch.current === current;
    setMode('loading'); setStatus(null); setImage(null); setConfirm(null); setUnavailable(false); setStopping(false);
    const refresh = async () => {
      if (!valid() || checking || document.visibilityState === 'hidden') return;
      checking = true;
      try {
        const value = await client.call('computer_sessions.status', { agent_id: agentId }) as Record<string, unknown>;
        if (!valid()) return;
        if (!value || value.ok !== true || typeof value.active !== 'boolean') throw new Error('invalid status');
        if (expectedSession && (!value.active || value.session_id !== expectedSession)) {
          setImage(null); setStatus(null); setConfirm(null); setMode('denied'); onExpired?.(); return;
        }
        // A chat link stays pinned even for someone who also has direct
        // access. It cannot mount a panel that might select a newer session.
        if (value.view_mode !== 'masked' && !expectedSession) { setMode('direct'); return; }
        setMode('masked'); setUnavailable(false);
        if (!value.active) { previousSession = null; setStatus({ active: false }); setImage(null); setConfirm(null); return; }
        const id = value.session_id;
        if (typeof id !== 'string' || !id || id.length > 128 || /[\x00-\x1f]/.test(id)) throw new Error('invalid session');
        if (id !== previousSession) { setImage(null); previousSession = id; }
        setStatus({ active: true, session_id: id, state: value.state === 'paused' ? 'paused' : 'running' });
        setConfirm((old) => old === id ? old : null);
        try {
          const next = snapshot(await client.call('computer_sessions.snapshot', { agent_id: agentId, session_id: id }), id);
          if (!valid()) return;
          setImage(next); setUnavailable(!next);
        } catch {
          if (valid()) { setImage(null); setUnavailable(true); }
        }
      } catch {
        if (valid()) { setMode('denied'); setStatus(null); setImage(null); setConfirm(null); }
      } finally { checking = false; }
    };
    refreshRef.current = refresh;
    void refresh();
    const timer = setInterval(() => void refresh(), 2000);
    const visible = () => { if (document.visibilityState !== 'hidden') void refresh(); };
    document.addEventListener('visibilitychange', visible);
    return () => { alive = false; ++epoch.current; clearInterval(timer); document.removeEventListener('visibilitychange', visible); };
  }, [agentId, expectedSession, onExpired]);

  const stop = async () => {
    if (!confirm || confirm !== status?.session_id || stopping) return;
    const current = epoch.current;
    const expected = confirm;
    setStopping(true);
    try {
      await client.call('computer_sessions.stop', { agent_id: agentId, session_id: expected });
      if (current === epoch.current) { setImage(null); setConfirm(null); await refreshRef.current(); }
    } catch {
      if (current === epoch.current) { setImage(null); setUnavailable(true); }
    } finally { if (current === epoch.current) setStopping(false); }
  };

  if (mode === 'direct') return <ComputerSessionPanel agentId={agentId} />;
  if (mode === 'loading') return <p role="status" className="p-6 text-sm text-muted-foreground">{t('loading')}</p>;
  if (mode === 'denied') return <p role="alert" className="p-6 text-sm text-muted-foreground">{t('denied')}</p>;
  return <section className="mx-auto w-full max-w-[1440px] space-y-4 p-4 sm:p-6" aria-label={t('title')}>
    <header className="flex flex-wrap items-start justify-between gap-4 border-b pb-4">
      <div className="min-w-0 space-y-1">
        <h2 className="flex flex-wrap items-center gap-2 font-medium"><Eye className="size-4" aria-hidden="true" />{t('title')}<span className="min-w-0 break-all font-mono text-xs text-muted-foreground">· {agentId}</span></h2>
        <p className="max-w-2xl text-sm text-muted-foreground">{t('notice')}</p>
      </div>
      {status?.active && (confirm ? <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm">{t('confirm')}</span>
        <Button size="sm" variant="destructive" disabled={stopping} onClick={() => void stop()}>{t('yes')}</Button>
        <Button size="sm" variant="ghost" disabled={stopping} onClick={() => setConfirm(null)}>{t('cancel')}</Button>
      </div> : <Button size="sm" variant="outline" onClick={() => setConfirm(status.session_id ?? null)}><Square />{t('stop')}</Button>)}
    </header>
    {!status?.active && <p className="py-8 text-sm text-muted-foreground">{t('inactive')}</p>}
    {unavailable && <p role="status" className="text-sm text-muted-foreground">{t('unavailable')}</p>}
    {image && <figure className="space-y-2">
      <img src={image.src} alt={t('image')} className="block h-auto max-h-[75vh] w-full object-contain object-left-top" />
      <figcaption className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <time dateTime={new Date(image.capturedAt).toISOString()}>{intl.formatTime(image.capturedAt, { hour: '2-digit', minute: '2-digit', second: '2-digit' })}</time>
        {image.stale && <span role="status">{t('stale')}</span>}
        {image.fullyMasked && <span role="status">{t('fullyMasked')}</span>}
      </figcaption>
    </figure>}
  </section>;
}
