import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Activity,
  MessagesSquare,
  SendHorizonal,
  UserRound,
  Loader2,
  FileDiff,
  FolderOpen,
  Package,
} from 'lucide-react';
import {
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  Empty,
  ActorAvatar,
} from '@/components/mds';
import { useAuthStore } from '@/stores/auth-store';
import { timeAgo } from '@/lib/format';
import type { ActivityEvent, TaskComment } from '@/lib/api';
import type { AssigneeOption } from './AssigneePopover';
import { TaskChangesList } from './TaskChangesPanel';
import { TaskArtifactsList } from './TaskArtifactsPanel';
import { TaskFilesPanel } from './TaskFilesPanel';
import { useTaskArtifacts, useTaskChanges } from './useTaskEvidence';

/**
 * TaskBottomTabs — the I-2a four-tab footer on the detail page
 * (`DESIGN-dashboard-ux-workbuddy-2026-08.md` §3.2), superseding the earlier
 * two-tab (對話／活動) and interim four-tab (產物／對話／活動／變更) layouts.
 *
 * Tab order matches the design doc's mockup exactly — 產物 first ("what the
 * assigner came for"), then 檔案, 變更, 過程:
 *
 *  · 產物 (Artifacts, I-2b): the curated hand-over — what this task actually
 *    delivered. Default-selected tab, per "產物置首".
 *  · 檔案 (Files, I-2a new): the raw `attachments/` inventory for this task,
 *    grouped 收到的／做出來的／來源不明 — see `TaskFilesPanel`.
 *  · 變更 (Changes, WP-F/P2-c): the file effects this task's rounds actually
 *    left behind, shared verbatim with the Inbox approval card's diff tab.
 *  · 過程 (Process): the design doc merges 「現行「活動」＋「對話」」 into one
 *    chronological stream — this is that merge, replacing the old separate
 *    Discussion/Activity tabs (the discussion tab already interleaved the two;
 *    only the redundant activity-only view is now gone).
 *
 * Doc note (I-2a scope decision): the design's ASCII mockup separately
 * annotates 描述／子任務／雙時鐘／輪次時間軸 (above the tab strip) as
 * "現有內容不動" (unchanged), while its prose says 過程 also absorbs the
 * round timeline. Those two parts of the same doc pull in different
 * directions. This implementation keeps the round-by-round timeline where it
 * already is, above the tabs in `TaskDetailPage` — unchanged, per the
 * diagram — because that keeps judge verdicts and rejection feedback
 * unconditionally visible without an extra click, which strengthens rather
 * than weakens the "審批動線不許變長" requirement. See the WP-5F report for
 * the full discrepancy note.
 *
 * ## Badge / fetch strategy
 *
 * 產物 and 變更 fetch EAGERLY (via `useTaskArtifacts`/`useTaskChanges`, not
 * gated by tab selection) so their counts can badge the tab before it is ever
 * opened — "分頁帶計數 badge 幫助使用者判斷要不要點進去" only works if the
 * count is already known. Both hooks feed the same reusable
 * `TaskArtifactsList`/`TaskChangesList` pure components the third/fourth-wave
 * panels already export, so the fetch is lifted without duplicating any
 * row-rendering logic and without firing the RPC twice. 檔案 has no design-
 * mandated badge, so it stays lazy (`TaskFilesPanel` fetches only once
 * opened). Every `TabsPanel` sets `keepMounted` so switching tabs never
 * unmounts a panel — no re-fetch, no lost scroll position, per I-2a's
 * "分頁切換保留各分頁捲動位置與載入狀態" requirement.
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

type BottomTab = 'artifacts' | 'files' | 'changes' | 'process';

export function TaskBottomTabs({
  taskId,
  agentId,
  events,
  comments,
  agents,
  onAddComment,
  currentUserId,
  currentUserName,
}: {
  /** Enables the 產物 / 檔案 / 變更 tabs. Omit and none of the three render —
   *  only 過程 (which needs no task id) is left. */
  taskId?: string;
  /** The task's assignee (`assigned_to`) — resolves the 檔案 tab's
   *  `/api/files` bucket. Omit/empty and that tab shows an honest
   *  "no assigned AI staff member" state instead of guessing. */
  agentId?: string | null;
  events: ReadonlyArray<ActivityEvent>;
  comments: ReadonlyArray<TaskComment>;
  agents: ReadonlyArray<AssigneeOption>;
  onAddComment: (body: string) => Promise<void> | void;
  currentUserId?: string;
  currentUserName?: string;
}) {
  const intl = useIntl();
  const jwt = useAuthStore((s) => s.jwt);
  // I-2a: 產物 first — "產物置首", the design doc's own framing that this is
  // what the assigner actually came for.
  const [tab, setTab] = useState<BottomTab>('artifacts');
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  // 檔案 has no badge, so unlike artifacts/changes it stays lazily mounted —
  // fetched only once the user actually opens it, then kept mounted (see
  // `keepMounted` below) so a later switch away and back never re-fetches.
  const [filesVisited, setFilesVisited] = useState(false);
  useEffect(() => {
    if (tab === 'files') setFilesVisited(true);
  }, [tab]);

  const artifactsState = useTaskArtifacts(taskId);
  const changesState = useTaskChanges(taskId);

  // Merge comments + activity into one chronological (oldest → newest) stream
  // — this IS the 過程 tab's content.
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
    <Tabs variant="line" value={tab} onValueChange={(v) => setTab(v as BottomTab)}>
      {/* Mobile: many labels in a narrow viewport → horizontal swipe instead
       *  of wrap/clip. Tabs keep their intrinsic width (`shrink-0`) so the
       *  strip overflows into a scroll container rather than squeezing. */}
      <TabsList className="overflow-x-auto border-b border-surface-border">
        {taskId && (
          <TabsTab value="artifacts" className="shrink-0">
            <Package />
            {intl.formatMessage({ id: 'tasks.tab.artifacts' })}
            {count(artifactsState.data?.artifacts?.length ?? 0)}
          </TabsTab>
        )}
        {taskId && (
          <TabsTab value="files" className="shrink-0">
            <FolderOpen />
            {intl.formatMessage({ id: 'tasks.tab.files' })}
          </TabsTab>
        )}
        {taskId && (
          <TabsTab value="changes" className="shrink-0">
            <FileDiff />
            {intl.formatMessage({ id: 'tasks.tab.changes' })}
            {count(changesState.data?.changes?.length ?? 0)}
          </TabsTab>
        )}
        <TabsTab value="process" className="shrink-0">
          <Activity />
          {intl.formatMessage({ id: 'tasks.tab.process' })}
          {count(merged.length)}
        </TabsTab>
      </TabsList>

      {/* keepMounted on every panel: switching tabs must never unmount one —
       *  that would both re-fire the eager artifacts/changes fetch (defeats
       *  the badge-sharing point above) and reset each panel's scroll
       *  position (I-2a requirement). */}
      {taskId && (
        <TabsPanel value="artifacts" keepMounted>
          <TaskArtifactsList
            artifacts={artifactsState.data?.artifacts ?? []}
            inferredCount={artifactsState.data?.inferred_count ?? 0}
            truncated={artifactsState.data?.truncated ?? false}
            loading={artifactsState.loading}
            error={artifactsState.error}
            jwt={jwt}
          />
        </TabsPanel>
      )}

      {taskId && (
        <TabsPanel value="files" keepMounted>
          {filesVisited && <TaskFilesPanel taskId={taskId} agentId={agentId} />}
        </TabsPanel>
      )}

      {taskId && (
        <TabsPanel value="changes" keepMounted>
          <TaskChangesList
            changes={changesState.data?.changes ?? []}
            distinctPaths={changesState.data?.distinct_paths ?? 0}
            truncated={changesState.data?.truncated ?? false}
            loading={changesState.loading}
            error={changesState.error}
          />
        </TabsPanel>
      )}

      <TabsPanel value="process" keepMounted>
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
    </Tabs>
  );
}
