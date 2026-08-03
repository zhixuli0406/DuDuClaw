import { useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { MessagesSquare, Plus, Search, ChevronLeft, ChevronRight } from 'lucide-react';
import { api, type ChatSessionSummary } from '@/lib/api';
import { toast } from '@/lib/toast';
import { timeAgo } from '@/lib/format';
import { sessionChannel, isConversationSession } from '@/lib/session-channel';
import { hasMinRole } from '@/lib/roles';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useChatStore } from '@/stores/chat-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useConversationsStore } from '@/stores/conversations-store';
import {
  ActorAvatar,
  Badge,
  Button,
  CollectionPageHeader,
  CollectionPageState,
  Input,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  toCompactAction,
} from '@/components/mds';

/**
 * The full conversation history page (2026-08-04, D17). The sidebar zone shows
 * the five most recent threads and links here for everything else; the chat page
 * no longer carries its own list, so this is the one place that answers "where
 * is that conversation from last week".
 *
 * Listing is `chat.sessions.list`, which is fail-closed server-side: admins may
 * enumerate every AI employee, everyone else must name one. The RPC has no
 * offset parameter, so it hands back one newest-first page (200 max) and the
 * search / employee filter / paging below are client-side over that window —
 * honest about its limit rather than pretending to be infinite.
 */

/** Server cap for one listing call (`CHAT_SESSIONS_LIST_MAX_LIMIT`). */
const FETCH_LIMIT = 200;
/** Rows per page. */
const PAGE_SIZE = 20;

const COLUMNS = 'minmax(0,2.2fr) minmax(0,1fr) minmax(0,0.7fr) minmax(0,0.5fr) minmax(0,0.9fr)';

type LoadState = 'loading' | 'ready' | 'error';

export function ConversationsPage() {
  const intl = useIntl();
  const navigate = useNavigate();

  const role = useAuthStore((s) => s.user?.role);
  const canListAll = hasMinRole(role, 'admin');
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const connectionState = useConnectionStore((s) => s.state);
  const activeSessionId = useChatStore((s) => s.sessionId);
  const selectedAgentId = useChatStore((s) => s.selectedAgentId);
  const resume = useConversationsStore((s) => s.resume);
  const startNew = useConversationsStore((s) => s.startNew);

  const [sessions, setSessions] = useState<readonly ChatSessionSummary[]>([]);
  const [state, setState] = useState<LoadState>('loading');
  const [query, setQuery] = useState('');
  const [agentFilter, setAgentFilter] = useState<string>('all');
  const [page, setPage] = useState(0);
  /** Bumped by the retry button so a failed listing can be re-attempted. */
  const [reloadNonce, setReloadNonce] = useState(0);

  const mainAgentId = useMemo(
    () => agents.find((a) => a.role === 'main')?.name ?? null,
    [agents],
  );
  // The scope a non-admin listing falls back to when no employee is picked.
  const defaultAgentId = selectedAgentId ?? mainAgentId;

  useEffect(() => {
    if (connectionState === 'authenticated') fetchAgents();
  }, [connectionState, fetchAgents]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;

    const scoped = agentFilter !== 'all' ? agentFilter : canListAll ? null : defaultAgentId;
    // Non-admin with nothing to scope to: the RPC would reject an unscoped
    // listing, so say "nothing here" rather than showing a failure.
    if (!canListAll && !scoped) {
      setSessions([]);
      setState('ready');
      return;
    }

    setState('loading');
    api.chatSessions
      .list(scoped ? { agent_id: scoped, limit: FETCH_LIMIT } : { limit: FETCH_LIMIT })
      .then((res) => {
        if (cancelled) return;
        setSessions((res?.sessions ?? []).filter((s) => isConversationSession(s.session_id)));
        setState('ready');
      })
      .catch(() => {
        if (!cancelled) setState('error');
      });

    return () => {
      cancelled = true;
    };
  }, [connectionState, canListAll, agentFilter, defaultAgentId, reloadNonce]);

  const agentLabel = (agentId: string) =>
    agents.find((a) => a.name === agentId)?.display_name ?? agentId;

  const untitled = intl.formatMessage({ id: 'webchat.history.untitled' });

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return sessions;
    return sessions.filter((s) => {
      const channel = sessionChannel(s.session_id);
      const haystack = [
        s.title || untitled,
        agentLabel(s.agent_id),
        s.agent_id,
        channel?.label ?? '',
      ]
        .join(' ')
        .toLowerCase();
      return haystack.includes(needle);
    });
    // `agents` participates through agentLabel; re-run when the roster lands.
  }, [sessions, query, agents, untitled]);

  // Any change to what is being listed puts you back on the first page —
  // otherwise a narrowed filter can strand the view on an empty page 4.
  useEffect(() => {
    setPage(0);
  }, [query, agentFilter, sessions]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const safePage = Math.min(page, pageCount - 1);
  const visible = filtered.slice(safePage * PAGE_SIZE, safePage * PAGE_SIZE + PAGE_SIZE);

  const open = async (session: ChatSessionSummary) => {
    if (!(await resume(session))) {
      toast.error(intl.formatMessage({ id: 'webchat.history.loadFailed' }));
      return;
    }
    navigate('/chat');
  };

  // `'all'` is the untouched state for everyone: an admin sees every employee,
  // a non-admin sees the one the server scopes them to. Either way it is not a
  // narrowing the user chose, so it must not read as "your filter hid things".
  const isFiltered = query.trim() !== '' || agentFilter !== 'all';
  const clearFilters = () => {
    setQuery('');
    setAgentFilter('all');
  };

  const newConversation = (
    <Button
      variant="brand"
      size="sm"
      onClick={() => {
        startNew();
        navigate('/chat');
      }}
    >
      <Plus />
      {intl.formatMessage({ id: 'nav.newChat' })}
    </Button>
  );

  // Escapes the shell's ambient padding so the header and the row grid span the
  // full width, matching every other collection page (AgentsPage / PlansPage).
  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        icon={MessagesSquare}
        title={intl.formatMessage({ id: 'nav.conversations' })}
        count={filtered.length}
        description={intl.formatMessage({ id: 'conversations.description' })}
        action={newConversation}
        actionCompact={toCompactAction(newConversation)}
      />

      {/* Filter bar — free-text search + employee scope. */}
      <div className="flex flex-wrap items-center gap-2 border-b border-surface-border px-5 py-2.5">
        <div className="relative min-w-0 flex-1 sm:max-w-xs">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={intl.formatMessage({ id: 'conversations.search.placeholder' })}
            aria-label={intl.formatMessage({ id: 'conversations.search.placeholder' })}
            className="pl-8"
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        {/* "所有 AI 員工" is only offered to viewers who may actually enumerate
            everyone. For anyone else the listing is scoped server-side to one
            employee, so the trigger names that employee instead of promising a
            breadth the RPC would refuse (2026-08-04 review, FINDING-4). */}
        <Select value={agentFilter} onValueChange={(v) => setAgentFilter(String(v))}>
          <SelectTrigger className="w-48" aria-label={intl.formatMessage({ id: 'conversations.filter.agent' })}>
            <SelectValue>
              {agentFilter !== 'all'
                ? agentLabel(agentFilter)
                : canListAll
                  ? intl.formatMessage({ id: 'conversations.filter.allAgents' })
                  : defaultAgentId
                    ? agentLabel(defaultAgentId)
                    : intl.formatMessage({ id: 'conversations.filter.agent' })}
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            {canListAll && (
              <SelectItem value="all">
                {intl.formatMessage({ id: 'conversations.filter.allAgents' })}
              </SelectItem>
            )}
            {agents.map((a) => (
              <SelectItem key={a.name} value={a.name}>
                {a.display_name || a.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {state === 'loading' ? (
        <CollectionPageState state="loading" title={intl.formatMessage({ id: 'common.loading' })} />
      ) : state === 'error' ? (
        <CollectionPageState
          state="error"
          icon={MessagesSquare}
          title={intl.formatMessage({ id: 'webchat.history.error' })}
          action={
            <Button variant="outline" size="sm" onClick={() => setReloadNonce((n) => n + 1)}>
              {intl.formatMessage({ id: 'webchat.history.retry' })}
            </Button>
          }
        />
      ) : filtered.length === 0 ? (
        // Two different empty states: "you have no history" wants to know how
        // history gets made; "your filter matched nothing" wants the filter
        // undone. Telling the second one to go start a conversation is wrong
        // advice (2026-08-04 review, FINDING-5).
        isFiltered ? (
          <CollectionPageState
            state="empty"
            icon={MessagesSquare}
            title={intl.formatMessage({ id: 'conversations.empty.filtered' })}
            description={intl.formatMessage({ id: 'conversations.empty.filtered.desc' })}
            action={
              <Button variant="outline" size="sm" onClick={clearFilters}>
                {intl.formatMessage({ id: 'conversations.filter.clear' })}
              </Button>
            }
          />
        ) : (
          <CollectionPageState
            state="empty"
            icon={MessagesSquare}
            title={intl.formatMessage({ id: 'webchat.history.empty' })}
            description={intl.formatMessage({ id: 'conversations.empty.desc' })}
          />
        )
      ) : (
        <>
          <ListGridContainer
            columns={COLUMNS}
            className="min-h-[40vh]"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>
                  {intl.formatMessage({ id: 'conversations.col.title' })}
                </ListGridHeaderCell>
                <ListGridHeaderCell>
                  {intl.formatMessage({ id: 'conversations.col.agent' })}
                </ListGridHeaderCell>
                <ListGridHeaderCell>
                  {intl.formatMessage({ id: 'conversations.col.channel' })}
                </ListGridHeaderCell>
                <ListGridHeaderCell className="justify-end">
                  {intl.formatMessage({ id: 'conversations.col.turns' })}
                </ListGridHeaderCell>
                <ListGridHeaderCell className="justify-end">
                  {intl.formatMessage({ id: 'conversations.col.lastActive' })}
                </ListGridHeaderCell>
              </ListGridHeader>
            }
          >
            {visible.map((s) => {
              const channel = sessionChannel(s.session_id);
              const title = s.title || untitled;
              const owner = agentLabel(s.agent_id);
              return (
                <ListGridRow
                  key={s.session_id}
                  selected={s.session_id === activeSessionId}
                  onClick={() => void open(s)}
                >
                  <ListGridCell>
                    <button
                      type="button"
                      onClick={() => void open(s)}
                      className="min-w-0 truncate text-left text-sm text-foreground hover:underline"
                      title={title}
                    >
                      {title}
                    </button>
                  </ListGridCell>
                  <ListGridCell className="gap-2">
                    <ActorAvatar actorType="agent" size="xs" name={owner} />
                    <span className="truncate text-sm text-muted-foreground" title={owner}>
                      {owner}
                    </span>
                  </ListGridCell>
                  <ListGridCell hideBelow>
                    {channel && <Badge variant="secondary">{channel.label}</Badge>}
                  </ListGridCell>
                  <ListGridCell className="justify-end">
                    <span className="font-mono text-xs tabular-nums text-muted-foreground">
                      {s.turns}
                    </span>
                  </ListGridCell>
                  <ListGridCell className="justify-end">
                    <span
                      className="truncate text-xs text-muted-foreground"
                      title={new Date(s.last_active).toLocaleString()}
                    >
                      {timeAgo(s.last_active)}
                    </span>
                  </ListGridCell>
                </ListGridRow>
              );
            })}
          </ListGridContainer>

          {pageCount > 1 && (
            <div className="flex items-center justify-end gap-2 border-t border-surface-border px-5 py-2.5">
              <span className="font-mono text-xs tabular-nums text-muted-foreground">
                {intl.formatMessage(
                  { id: 'conversations.page' },
                  { page: safePage + 1, total: pageCount },
                )}
              </span>
              <Button
                variant="outline"
                size="icon-sm"
                disabled={safePage === 0}
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                aria-label={intl.formatMessage({ id: 'conversations.page.prev' })}
              >
                <ChevronLeft />
              </Button>
              <Button
                variant="outline"
                size="icon-sm"
                disabled={safePage >= pageCount - 1}
                onClick={() => setPage((p) => Math.min(pageCount - 1, p + 1))}
                aria-label={intl.formatMessage({ id: 'conversations.page.next' })}
              >
                <ChevronRight />
              </Button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
