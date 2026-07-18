import { useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { MessagesSquare, Activity, SendHorizonal, UserRound, Loader2 } from 'lucide-react';
import {
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  Empty,
  ActorAvatar,
} from '@/components/mds';
import { timeAgo } from '@/lib/format';
import type { ActivityEvent, TaskComment } from '@/lib/api';
import type { AssigneeOption } from './AssigneePopover';

/**
 * TaskBottomTabs — the spec §5.3 式1 footer tabs on the detail page (line variant).
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
 *  agent avatars. */
function PersonMarker() {
  return (
    <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-brand/15 text-brand">
      <UserRound className="size-3.5" aria-hidden="true" />
    </span>
  );
}

/** A small mono timestamp token used across the timelines. */
function Ago({ ts }: { ts: string }) {
  return <span className="font-mono text-xs tabular-nums text-muted-foreground">{timeAgo(ts)}</span>;
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
    return <Empty icon={Activity} title={intl.formatMessage({ id: 'tasks.activity.empty' })} />;
  }
  return (
    <ol className="space-y-3 py-2">
      {events.map((ev) => {
        const agent = agents.find((a) => a.name === ev.agent_id);
        return (
          <li key={ev.id} className="flex items-start gap-2.5">
            <ActorAvatar actorType="agent" size="md" name={agent?.display_name ?? ev.agent_id} />
            <div className="min-w-0 flex-1">
              <p className="text-sm text-foreground">{ev.summary}</p>
              <div className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground">
                <span className="truncate">{agent?.display_name ?? ev.agent_id}</span>
                <Ago ts={ev.timestamp} />
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

  const count = (n: number) =>
    n > 0 ? <span className="font-mono text-xs tabular-nums text-muted-foreground">{n}</span> : null;

  return (
    <Tabs variant="line" value={tab} onValueChange={(v) => setTab(v as 'discussion' | 'activity')}>
      <TabsList className="border-b border-surface-border">
        <TabsTab value="discussion">
          <MessagesSquare />
          {intl.formatMessage({ id: 'tasks.tab.discussion' })}
          {count(comments.length)}
        </TabsTab>
        <TabsTab value="activity">
          <Activity />
          {intl.formatMessage({ id: 'tasks.tab.activity' })}
          {count(events.length)}
        </TabsTab>
      </TabsList>

      <TabsPanel value="discussion">
        <div className="space-y-3">
          {merged.length === 0 ? (
            <Empty icon={MessagesSquare} title={intl.formatMessage({ id: 'tasks.discussion.empty' })} />
          ) : (
            <ol className="space-y-3 py-2">
              {merged.map((it) =>
                it.kind === 'comment' ? (
                  <li key={`c-${it.comment.id}`} className="flex items-start gap-2.5">
                    <PersonMarker />
                    <div className="min-w-0 flex-1">
                      <p className="whitespace-pre-wrap text-sm text-foreground">{it.comment.body}</p>
                      <div className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground">
                        <span className="truncate">{authorLabel(it.comment.author_user)}</span>
                        <Ago ts={it.comment.created_at} />
                      </div>
                    </div>
                  </li>
                ) : (
                  <li key={`a-${it.event.id}`} className="flex items-start gap-2.5">
                    <ActorAvatar
                      actorType="agent"
                      size="md"
                      name={agents.find((a) => a.name === it.event.agent_id)?.display_name ?? it.event.agent_id}
                    />
                    <div className="min-w-0 flex-1">
                      <p className="text-sm text-foreground">{it.event.summary}</p>
                      <div className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground">
                        <span className="truncate">
                          {agents.find((a) => a.name === it.event.agent_id)?.display_name ?? it.event.agent_id}
                        </span>
                        <Ago ts={it.event.timestamp} />
                      </div>
                    </div>
                  </li>
                ),
              )}
            </ol>
          )}

          {/* Composer — post a comment (⌘/Ctrl+Enter to send). */}
          <div className="flex items-end gap-2 rounded-lg border border-input bg-transparent px-3 py-2 transition-colors focus-within:border-ring dark:bg-input/30">
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={handleKeyDown}
              rows={1}
              placeholder={intl.formatMessage({ id: 'tasks.comment.placeholder' })}
              aria-label={intl.formatMessage({ id: 'tasks.comment.placeholder' })}
              className="min-h-[1.5rem] min-w-0 flex-1 resize-none bg-transparent text-sm text-foreground placeholder:text-muted-foreground focus:outline-none"
            />
            <button
              type="button"
              onClick={() => void submit()}
              disabled={!draft.trim() || sending}
              title={intl.formatMessage({ id: 'tasks.comment.send' })}
              aria-label={intl.formatMessage({ id: 'tasks.comment.send' })}
              className="shrink-0 rounded-md p-1 text-brand transition-colors hover:bg-brand/10 disabled:cursor-not-allowed disabled:text-muted-foreground/50"
            >
              {sending ? (
                <Loader2 className="size-4 animate-spin" aria-hidden="true" />
              ) : (
                <SendHorizonal className="size-4" aria-hidden="true" />
              )}
            </button>
          </div>
        </div>
      </TabsPanel>

      <TabsPanel value="activity">
        <ActivityTimeline events={events} agents={agents} />
      </TabsPanel>
    </Tabs>
  );
}
