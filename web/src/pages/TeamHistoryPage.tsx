import { useEffect, useMemo, useRef, useState } from 'react';
import { useIntl, type IntlShape } from 'react-intl';
import { useSearchParams } from 'react-router';
import { Eye, Loader2, RefreshCw, ShieldCheck } from 'lucide-react';
import { api, type MyTeam, type TeamHistoryClass, type TeamHistoryList, type TeamHistoryPart, type TeamHistoryTranscript } from '@/lib/api';
import { Button } from '@/components/mds';
import { cn } from '@/lib/utils';

/** Content is rendered as plain React text. No Markdown renderer, embedded
 * media, download URL, shared chat store or resumable conversation is used. */
export function TeamHistoryPage() {
  const intl = useIntl();
  const [query] = useSearchParams();
  const [team, setTeam] = useState<MyTeam | null>(null);
  const [teamLoading, setTeamLoading] = useState(true);
  const [agent, setAgent] = useState(query.get('agent') ?? '');
  const [report, setReport] = useState(query.get('report') ?? '');
  const [kind, setKind] = useState<TeamHistoryClass | ''>('');
  const [selected, setSelected] = useState('');
  const [refresh, setRefresh] = useState(0);
  const [page, setPage] = useState<{ key: string; data: TeamHistoryList } | null>(null);
  const [detail, setDetail] = useState<{ key: string; data: TeamHistoryTranscript } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [teamError, setTeamError] = useState<string | null>(null);
  const [listLoading, setListLoading] = useState(false);
  const [detailLoading, setDetailLoading] = useState(false);
  const [moreLoading, setMoreLoading] = useState(false);
  const epoch = useRef(0);
  const msg = (id: string) => intl.formatMessage({ id: `teamHistory.${id}` });
  const classifyError = (e: unknown) => {
    const code = e && typeof e === 'object' && 'code' in e ? String(e.code) : '';
    return msg(code === 'team_history_off' ? 'off' : code === 'rate_limited' ? 'rateLimited' : 'unavailable');
  };

  useEffect(() => {
    let active = true;
    setTeamLoading(true);
    setTeam(null);
    setTeamError(null);
    api.users.myTeam().then(data => { if (active) setTeam(data); })
      .catch(() => { if (active) setTeamError(intl.formatMessage({ id: 'teamHistory.unavailable' })); })
      .finally(() => { if (active) setTeamLoading(false); });
    return () => { active = false; };
  }, [refresh, intl]);

  const agents = useMemo(() => {
    const all = team?.members.flatMap(m => m.agents.filter(a => a.history_readable === true)) ?? [];
    return [...new Map(all.map(a => [a.name, a])).values()];
  }, [team]);
  const chosenAgent = agents.some(a => a.name === agent) ? agent : agents[0]?.name ?? '';
  const reports = team?.members.filter(m => m.agents.some(a => a.name === chosenAgent && a.history_readable === true)) ?? [];
  const chosenReport = reports.some(m => m.id === report) ? report : '';
  const scope = JSON.stringify([chosenAgent, chosenReport, kind, refresh]);
  const currentPage = page?.key === scope ? page.data : null;
  const detailKey = JSON.stringify([scope, selected]);
  const currentDetail = detail?.key === detailKey ? detail.data : null;

  useEffect(() => {
    const ticket = ++epoch.current;
    setSelected('');
    setPage(null);
    setDetail(null);
    setError(null);
    setMoreLoading(false);
    if (!chosenAgent || teamLoading) return;
    setListLoading(true);
    api.users.teamHistory.list({ agent_id: chosenAgent, ...(chosenReport ? { report_user_id: chosenReport } : {}), ...(kind ? { class: kind } : {}), limit: 50 })
      .then(data => { if (ticket === epoch.current) setPage({ key: scope, data }); })
      .catch(e => { if (ticket === epoch.current) setError(classifyError(e)); })
      .finally(() => { if (ticket === epoch.current) setListLoading(false); });
    return () => { ++epoch.current; };
    // `scope` includes every request input. The ticket fences stale responses.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope, teamLoading]);

  useEffect(() => {
    let active = true;
    setDetail(null);
    if (!selected || !currentPage) return;
    setDetailLoading(true);
    api.users.teamHistory.get(selected)
      .then(data => {
        if (!active) return;
        if (data.session_id !== selected || data.agent_id !== chosenAgent || data.read_only !== true) throw new Error('invalid conversation');
        if (data.since !== currentPage.since) { reload(); return; }
        setDetail({ key: detailKey, data });
      })
      .catch(e => { if (active) { ++epoch.current; setPage(null); setSelected(''); setError(classifyError(e)); } })
      .finally(() => { if (active) setDetailLoading(false); });
    return () => { active = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detailKey]);

  const reload = () => { ++epoch.current; setTeamLoading(true); setTeam(null); setPage(null); setDetail(null); setSelected(''); setError(null); setRefresh(n => n + 1); };
  useEffect(() => {
    const focus = () => reload();
    window.addEventListener('focus', focus);
    return () => window.removeEventListener('focus', focus);
  }, []);

  async function more() {
    if (!currentPage?.next_before || moreLoading) return;
    const ticket = epoch.current;
    setMoreLoading(true);
    try {
      const data = await api.users.teamHistory.list({ agent_id: chosenAgent, ...(chosenReport ? { report_user_id: chosenReport } : {}), ...(kind ? { class: kind } : {}), before: currentPage.next_before, limit: 50 });
      if (ticket !== epoch.current) return;
      if (data.since !== currentPage.since) { reload(); return; }
      setPage({ key: scope, data: { ...data, sessions: [...new Map([...currentPage.sessions, ...data.sessions].map(s => [s.session_id, s])).values()] } });
    } catch (e) {
      if (ticket === epoch.current) { setPage(null); setDetail(null); setSelected(''); setError(classifyError(e)); }
    } finally { if (ticket === epoch.current) setMoreLoading(false); }
  }
  const selectClass = 'w-full rounded-lg border border-input bg-background px-3 py-2 text-sm';

  return <div className="mx-auto w-full max-w-7xl space-y-4">
    <header className="flex items-center justify-between gap-3">
      <div className="flex items-center gap-2"><Eye className="size-5 text-muted-foreground" /><h1 className="text-base font-medium">{msg('title')}</h1></div>
      <Button variant="outline" size="sm" onClick={reload}><RefreshCw />{msg('refresh')}</Button>
    </header>
    <p className="flex items-start gap-2 rounded-lg bg-muted px-3 py-2 text-sm"><ShieldCheck className="mt-0.5 size-4 shrink-0" />{msg('readOnly')}</p>
    {(error || teamError) && <p role="alert" className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{error || teamError}</p>}
    {teamLoading ? <Loader2 aria-label={msg('loading')} className="mx-auto my-12 size-6 animate-spin" />
      : team?.history_enabled === false ? <p className="py-12 text-center text-sm text-muted-foreground">{msg('off')}</p>
      : !chosenAgent ? !teamError && <p className="py-12 text-center text-sm text-muted-foreground">{team?.history_enabled == null ? msg('unavailable') : msg('empty')}</p>
      : <>
        <div className="grid gap-3 sm:grid-cols-3">
          <label className="space-y-1 text-sm">{msg('agent')}<select aria-label={msg('agent')} className={selectClass} value={chosenAgent} onChange={e => { setAgent(e.target.value); setReport(''); setSelected(''); }}>
            {agents.map(a => <option key={a.name} value={a.name}>{a.display_name}</option>)}
          </select></label>
          <label className="space-y-1 text-sm">{msg('report')}<select aria-label={msg('report')} className={selectClass} value={chosenReport} onChange={e => { setReport(e.target.value); setSelected(''); }}>
            <option value="">{msg('allReports')}</option>{reports.map(r => <option key={r.id} value={r.id}>{r.display_name}</option>)}
          </select></label>
          <label className="space-y-1 text-sm">{msg('type')}<select aria-label={msg('type')} className={selectClass} value={kind} onChange={e => { setKind(e.target.value as TeamHistoryClass | ''); setSelected(''); }}>
            <option value="">{msg('allTypes')}</option><option value="report_webchat">{msg('reportWebchat')}</option><option value="external">{msg('external')}</option>
          </select></label>
        </div>
        {(currentPage?.ner === false || currentDetail?.class === 'external' && currentDetail.ner === false) && <p role="status" className="rounded-lg bg-warning/10 px-3 py-2 text-sm text-warning">{msg('noNer')}</p>}
        <div className="grid min-w-0 gap-4 lg:grid-cols-[minmax(16rem,1fr)_minmax(0,2fr)]">
          <section aria-label={msg('list')} className="min-w-0 space-y-2">
            {listLoading && <Loader2 aria-label={msg('loading')} className="mx-auto my-6 size-5 animate-spin" />}
            {currentPage?.sessions.length === 0 && <p className="py-8 text-sm text-muted-foreground">{msg('empty')}</p>}
            {currentPage?.sessions.map(s => <button type="button" key={s.session_id} onClick={() => { setError(null); setSelected(s.session_id); }} aria-pressed={selected === s.session_id}
              className={cn('w-full min-w-0 rounded-xl border border-surface-border p-3 text-left hover:bg-muted', selected === s.session_id && 'bg-surface-selected ring-1 ring-brand/40')}>
              <span className="block break-words text-sm font-medium">{localizedTitle(s.title, s.title_masked, intl) || msg('untitled')}</span>
              <span className="mt-1 block text-xs text-muted-foreground">{s.report?.display_name ?? (s.class === 'own' ? msg('you') : msg('external'))} · {s.channel ?? 'WebChat'} · {s.turns}</span>
              <time className="text-xs text-muted-foreground" dateTime={s.last_active}>{formatDate(s.last_active, intl.locale)}</time>
              {s.title_masked && <span className="ml-2 rounded bg-muted px-1.5 py-0.5 text-xs">{msg('masked')}</span>}
            </button>)}
            {currentPage?.next_before && <Button variant="outline" disabled={moreLoading} onClick={() => void more()}>{msg('more')}</Button>}
          </section>
          <section aria-label={msg('conversation')} className="min-w-0 space-y-3 rounded-xl border border-surface-border bg-card p-4">
            {detailLoading && selected && <Loader2 aria-label={msg('loading')} className="mx-auto my-8 size-5 animate-spin" />}
            {!selected && <p className="py-10 text-center text-sm text-muted-foreground">{msg('choose')}</p>}
            {currentDetail?.messages.map((message, i) => <article key={i} className={cn('max-w-full space-y-1 rounded-xl p-3', message.role === 'assistant' ? 'bg-muted' : 'bg-background')}>
              <header className="flex flex-wrap justify-between gap-2 text-xs text-muted-foreground"><span>{message.sender_label}</span><time dateTime={message.timestamp}>{formatDate(message.timestamp, intl.locale)}</time></header>
              <p className="whitespace-pre-wrap break-words text-sm leading-relaxed">{message.parts.map((part, n) => <HistoryPart key={n} part={part} />)}</p>
            </article>)}
          </section>
        </div>
      </>}
  </div>;
}

function formatDate(value: string, locale: string) {
  const stamp = new Date(value);
  return Number.isNaN(stamp.getTime()) ? '' : stamp.toLocaleString(locale);
}

function HistoryPart({ part }: { part: TeamHistoryPart }) {
  const intl = useIntl();
  if (part.t === 'text') return <>{part.v}</>;
  if (part.t === 'attachment') return <span className="rounded bg-muted px-2 py-0.5 text-muted-foreground">{intl.formatMessage({ id: part.kind === 'image' ? 'teamHistory.image' : 'teamHistory.attachment' })}{part.name ? ` · ${part.name}` : ''}</span>;
  const label = categoryLabel(intl, part.cat);
  return <span className="mx-0.5 inline-block rounded-md bg-brand/10 px-1.5 py-0.5 text-xs text-brand" title={intl.formatMessage({ id: 'teamHistory.maskHint' }, { category: label })}>{label} {part.n}</span>;
}

function categoryLabel(intl: IntlShape, category: string): string {
  const id = `teamHistory.category.${category}`;
  return id in intl.messages ? intl.formatMessage({ id }) : category;
}

/** Only translate the server's masked-title markers. An own conversation's
 * literal text must remain unchanged; unknown custom categories stay text. */
function localizedTitle(title: string, masked: boolean, intl: IntlShape): string {
  if (!masked) return title;
  return title.replace(/〔([A-Z][A-Z0-9_]*) (\d+)〕/g, (_value, category: string, n: string) =>
    `〔${categoryLabel(intl, category)} ${n}〕`);
}
