import { useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { MessagesSquare, Activity, SendHorizonal, UserRound, Loader2 } from 'lucide-react';
import { Tabs, Mono, EmptyState, CharacterAvatar, type TabItem } from '@/components/ui';
import { timeAgo } from '@/lib/format';
import type { ActivityEvent, TaskComment } from '@/lib/api';
import type { AssigneeOption } from './AssigneePopover';

/**
 * TaskBottomTabs — the §5.3(7) footer tabs on the detail page.
 *
 *  · 對話 (Discussion): a live timeline mixing human comments (L2) with the
 *    task's system activity, sorted by time. The composer posts a comment via
 *    `onAddComment`; comment rows lead with a person marker, activity rows keep
 *    the employee avatar.
 *  · 活動 (Activity): the task-filtered activity stream only.
 */

type TimelineItem =
  | { kind: 'comment'; ts: string; comment: TaskComment }
  | { kind: 'activity'; ts: string; event: ActivityEvent };

/** A person marker for human comment rows — visually distinct from the round
 *  character avatars used for agents. */
function PersonMarker() {
  return (
    <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-amber-500/15 text-amber-600 dark:text-amber-400">
      <UserRound className="h-3.5 w-3.5" aria-hidden="true" />
    </span>
  );
}

function ActivityTimeline({
  events,
  agents,
}: {
  events: ReadonlyArray<ActivityEvent>;
  agents: ReadonlyArray<AssigneeOption>;
}) {
  const intl = useIntl();
  if (events.length === 0) {
    return <EmptyState icon={Activity} title={intl.formatMessage({ id: 'tasks.activity.empty' })} />;
  }
  return (
    <ol className="space-y-3 py-2">
      {events.map((ev) => {
        const agent = agents.find((a) => a.name === ev.agent_id);
        return (
          <li key={ev.id} className="flex items-start gap-2.5">
            <CharacterAvatar agentId={ev.agent_id} name={agent?.display_name} size={24} animated={false} />
            <div className="min-w-0 flex-1">
              <p className="text-sm text-stone-700 dark:text-stone-200">{ev.summary}</p>
              <div className="mt-0.5 flex items-center gap-2 text-xs text-stone-400 dark:text-stone-500">
                <span className="truncate">{agent?.display_name ?? ev.agent_id}</span>
                <Mono className="text-[0.6875rem]">{timeAgo(ev.timestamp)}</Mono>
              </div>
            </div>
          </li>
        );
      })}
    </ol>
  );
}

export function TaskBottomTabs({
  events,
  comments,
  agents,
  onAddComment,
  currentUserId,
  currentUserName,
}: {
  events: ReadonlyArray<ActivityEvent>;
  comments: ReadonlyArray<TaskComment>;
  agents: ReadonlyArray<AssigneeOption>;
  onAddComment: (body: string) => Promise<void> | void;
  currentUserId?: string;
  currentUserName?: string;
}) {
  const intl = useIntl();
  const [tab, setTab] = useState<'discussion' | 'activity'>('discussion');
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);

  const items = useMemo<readonly TabItem[]>(
    () => [
      {
        id: 'discussion',
        label: intl.formatMessage({ id: 'tasks.tab.discussion' }),
        icon: MessagesSquare,
        badge: comments.length || undefined,
      },
      {
        id: 'activity',
        label: intl.formatMessage({ id: 'tasks.tab.activity' }),
        icon: Activity,
        badge: events.length || undefined,
      },
    ],
    [intl, events.length, comments.length],
  );

  // Merge comments + activity into one chronological (oldest → newest) stream.
  const merged = useMemo<readonly TimelineItem[]>(() => {
    const list: TimelineItem[] = [
      ...comments.map((c) => ({ kind: 'comment' as const, ts: c.created_at, comment: c })),
      ...events.map((e) => ({ kind: 'activity' as const, ts: e.timestamp, event: e })),
    ];
    return list.sort((a, b) => a.ts.localeCompare(b.ts));
  }, [comments, events]);

  const authorLabel = (authorUser: string) =>
    authorUser === currentUserId && currentUserName ? currentUserName : authorUser;

  const submit = async () => {
    const body = draft.trim();
    if (!body || sending) return;
    setSending(true);
    try {
      await onAddComment(body);
      setDraft('');
    } finally {
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void submit();
    }
  };

  return (
    <div>
      <Tabs items={items} value={tab} onChange={(id) => setTab(id as 'discussion' | 'activity')} />
      {tab === 'discussion' ? (
        <div className="space-y-3">
          {merged.length === 0 ? (
            <EmptyState icon={MessagesSquare} title={intl.formatMessage({ id: 'tasks.discussion.empty' })} />
          ) : (
            <ol className="space-y-3 py-2">
              {merged.map((it) =>
                it.kind === 'comment' ? (
                  <li key={`c-${it.comment.id}`} className="flex items-start gap-2.5">
                    <PersonMarker />
                    <div className="min-w-0 flex-1">
                      <p className="whitespace-pre-wrap text-sm text-stone-800 dark:text-stone-100">
                        {it.comment.body}
                      </p>
                      <div className="mt-0.5 flex items-center gap-2 text-xs text-stone-400 dark:text-stone-500">
                        <span className="truncate">{authorLabel(it.comment.author_user)}</span>
                        <Mono className="text-[0.6875rem]">{timeAgo(it.comment.created_at)}</Mono>
                      </div>
                    </div>
                  </li>
                ) : (
                  <li key={`a-${it.event.id}`} className="flex items-start gap-2.5">
                    <CharacterAvatar
                      agentId={it.event.agent_id}
                      name={agents.find((a) => a.name === it.event.agent_id)?.display_name}
                      size={24}
                      animated={false}
                    />
                    <div className="min-w-0 flex-1">
                      <p className="text-sm text-stone-700 dark:text-stone-200">{it.event.summary}</p>
                      <div className="mt-0.5 flex items-center gap-2 text-xs text-stone-400 dark:text-stone-500">
                        <span className="truncate">
                          {agents.find((a) => a.name === it.event.agent_id)?.display_name ?? it.event.agent_id}
                        </span>
                        <Mono className="text-[0.6875rem]">{timeAgo(it.event.timestamp)}</Mono>
                      </div>
                    </div>
                  </li>
                ),
              )}
            </ol>
          )}

          {/* Composer — post a comment (⌘/Ctrl+Enter to send). */}
          <div className="flex items-end gap-2 rounded-control border border-[var(--panel-border)] bg-[var(--panel-fill)] px-3 py-2 focus-within:border-amber-500/50">
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={handleKeyDown}
              rows={1}
              placeholder={intl.formatMessage({ id: 'tasks.comment.placeholder' })}
              aria-label={intl.formatMessage({ id: 'tasks.comment.placeholder' })}
              className="min-h-[1.5rem] min-w-0 flex-1 resize-none bg-transparent text-sm text-stone-800 placeholder:text-stone-400 focus:outline-none dark:text-stone-100 dark:placeholder:text-stone-500"
            />
            <button
              type="button"
              onClick={() => void submit()}
              disabled={!draft.trim() || sending}
              title={intl.formatMessage({ id: 'tasks.comment.send' })}
              aria-label={intl.formatMessage({ id: 'tasks.comment.send' })}
              className="shrink-0 rounded-[calc(var(--radius-control)-2px)] p-1 text-amber-600 hover:bg-amber-500/10 disabled:cursor-not-allowed disabled:text-stone-300 dark:text-amber-400 dark:disabled:text-stone-600"
            >
              {sending ? (
                <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
              ) : (
                <SendHorizonal className="h-4 w-4" aria-hidden="true" />
              )}
            </button>
          </div>
        </div>
      ) : (
        <ActivityTimeline events={events} agents={agents} />
      )}
    </div>
  );
}
