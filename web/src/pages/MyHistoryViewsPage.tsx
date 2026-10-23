import { useCallback, useEffect, useRef, useState } from 'react';
import { Eye, Loader2, RefreshCw } from 'lucide-react';
import { useIntl } from 'react-intl';
import { api, type MyHistoryView } from '@/lib/api';
import { Button } from '@/components/mds';

/** A self-service receipt page. Neither the request nor the rendered row
 * accepts an audience, session identifier or arbitrary audit details. */
export function MyHistoryViewsPage() {
  const intl = useIntl();
  const msg = (key: string) => intl.formatMessage({ id: `myHistoryViews.${key}` });
  const [views, setViews] = useState<MyHistoryView[]>([]);
  const [before, setBefore] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const epoch = useRef(0);
  const load = useCallback(async (cursor?: string) => {
    const current = ++epoch.current;
    setLoading(true); setFailed(false);
    if (!cursor) { setViews([]); setBefore(null); }
    try {
      const page = await api.users.myHistoryViews(cursor);
      if (current !== epoch.current) return;
      if (!Array.isArray(page.views)) throw new Error('invalid receipt response');
      // Closed presentation types: a future event needs a separate reviewed
      // API and UI change, never a raw event_type or details renderer.
      const rows = page.views.filter(row => row.type === 'webchat');
      setViews(old => cursor ? [...old, ...rows] : rows);
      setBefore(page.next_before);
    } catch {
      if (current === epoch.current) { setViews([]); setBefore(null); setFailed(true); }
    } finally { if (current === epoch.current) setLoading(false); }
  }, []);
  useEffect(() => {
    void load();
    const focus = () => { void load(); };
    window.addEventListener('focus', focus);
    return () => { ++epoch.current; window.removeEventListener('focus', focus); };
  }, [load]);

  return <div className="mx-auto w-full max-w-4xl space-y-5">
    <header className="flex items-center justify-between gap-3">
      <div className="flex items-center gap-2"><Eye className="size-5 text-muted-foreground" /><h1 className="text-base font-medium">{msg('title')}</h1></div>
      <Button variant="outline" size="sm" onClick={() => void load()}><RefreshCw />{msg('refresh')}</Button>
    </header>
    <p className="rounded-lg bg-muted px-4 py-3 text-sm leading-relaxed text-muted-foreground">{msg('scope')}</p>
    {failed && <p role="alert" className="rounded-lg bg-destructive/10 px-4 py-3 text-sm text-destructive">{msg('unavailable')}</p>}
    {!loading && !failed && views.length === 0 && <p className="py-10 text-center text-sm text-muted-foreground">{msg(before ? 'scanMore' : 'empty')}</p>}
    <ul className="space-y-3" aria-label={msg('list')}>
      {views.map((view, index) => <li key={index} className="flex flex-wrap items-start justify-between gap-3 rounded-xl border border-surface-border bg-card p-4">
        <div className="min-w-0 space-y-1">
          <p className="break-words text-sm font-medium">{view.viewer_display_name ?? msg('formerManager')}</p>
          <p className="break-words text-sm text-muted-foreground">{view.agent_display_name ?? msg('formerAgent')} · {msg('webchat')}</p>
        </div>
        <time className="text-xs text-muted-foreground" dateTime={view.timestamp}>{new Date(view.timestamp).toLocaleString(intl.locale)}</time>
      </li>)}
    </ul>
    {loading && <Loader2 aria-label={msg('loading')} className="mx-auto size-5 animate-spin" />}
    {before && <Button variant="outline" disabled={loading} onClick={() => void load(before)}>{msg('more')}</Button>}
  </div>;
}
