import { useIntl } from 'react-intl';
import { Eye, Loader2, Plus, RefreshCw, ShieldCheck } from 'lucide-react';
import { Button } from '@/components/mds';
import { useConversationsStore } from '@/stores/conversations-store';
import { useAgentsStore } from '@/stores/agents-store';
import { channelLabel } from '@/lib/session-channel';
import { HistoryPart } from './HistoryPart';

/** A separate view keeps external transcripts out of the chat/composer,
 * voice, citation, action-card and resumable-session state. */
export function ReadOnlyHistoryView() {
  const intl = useIntl();
  const { viewLoading: loading, viewError: error, readOnlyView: data, refreshView, startNew } = useConversationsStore();
  const staff = useAgentsStore(s=>s.agents.find(a=>a.name===data?.agent_id));
  const msg = (key: string) => intl.formatMessage({ id: `boundHistory.${key}` });
  return <section className="mx-auto flex h-full w-full max-w-4xl flex-col gap-5 overflow-y-auto p-6">
    <header className="flex flex-wrap items-center justify-between gap-3">
      <h1 className="flex items-center gap-2 text-base font-semibold"><Eye className="size-4" />{data ? msg('title') : intl.formatMessage({ id: 'webchat.history.title' })}</h1>
      <div className="flex gap-2"><Button variant="outline" size="sm" onClick={() => void refreshView()} disabled={loading}><RefreshCw className="size-3.5" />{msg('refresh')}</Button><Button size="sm" onClick={startNew}><Plus className="size-3.5" />{msg('new')}</Button></div>
    </header>
    {loading ? <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><Loader2 className="size-4 animate-spin" />{msg('loading')}</p>
      : error ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/5 p-4 text-sm">{msg('error')}</p>
      : data && <>
        <p className="text-sm text-muted-foreground">{staff?.display_name || data.agent_id} · {channelLabel(data.channel ?? '')}</p>
        <p className="rounded-xl bg-muted/40 p-4 text-sm text-muted-foreground">{msg('description')}</p>
        {data.ner === false && <p className="flex gap-2 rounded-xl border border-amber-500/25 bg-amber-500/5 p-4 text-sm"><ShieldCheck className="mt-0.5 size-4 shrink-0" />{msg('regexOnly')}</p>}
        <ol className="flex flex-col gap-4">{data.messages.map((message, index) => <li key={index} className="rounded-xl border border-border/60 p-4">
          <div className="mb-2 flex flex-wrap justify-between gap-2 text-xs text-muted-foreground"><span>{(message.role==='assistant' && staff?.display_name) || message.sender_label || intl.formatMessage({ id: message.role === 'assistant' ? 'boundHistory.staff' : 'boundHistory.customer' })}</span><time>{Number.isFinite(Date.parse(message.timestamp)) ? intl.formatDate(new Date(message.timestamp), { dateStyle: 'medium', timeStyle: 'short' }) : ''}</time></div>
          <p className="whitespace-pre-wrap break-words text-sm leading-relaxed">{message.parts?.map((part, n) => <HistoryPart key={n} part={part} showAttachmentNames={false} />)}</p>
        </li>)}</ol>
      </>}
  </section>;
}
