import { useEffect } from 'react';
import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { ChevronDown } from 'lucide-react';
import { cn } from '@/lib/utils';
import { toast } from '@/lib/toast';
import { sessionChannel } from '@/lib/session-channel';
import type { ChatSessionSummary } from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { useAgentsStore } from '@/stores/agents-store';
import { useChatStore } from '@/stores/chat-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useConversationsStore } from '@/stores/conversations-store';
import {
  SidebarGroup,
  SidebarMenu,
  SidebarMenuItem,
  sidebarMenuButtonVariants,
} from '@/components/mds';

/**
 * How many past conversations the rail lists. The client asked for 15 (newest
 * first) — enough to cover "what was I doing", short enough that the rest of
 * the nav stays reachable without scrolling past a wall of titles. Everything
 * older stays on the chat page's own list, which is unfiltered and searchable
 * by employee.
 */
const RAIL_LIMIT = 15;

/**
 * 對話紀錄 — the sidebar conversation-history zone (2026-07-30 client feedback).
 *
 * Sits directly under 新對話 and mirrors the New chat + Recents rail people
 * already know. Rows resume the stored session in place; the chat page's left
 * column keeps the full list (all employees, all channels), so this zone is a
 * shortcut, never the only way in.
 *
 * Renders nothing in the icon-only collapsed rail: fifteen indistinguishable
 * bubbles would be noise, and 新對話 is still one click away there.
 */
export function ConversationsZone({
  collapsed,
  sectionCollapsed,
  onToggle,
}: {
  collapsed: boolean;
  sectionCollapsed: boolean;
  onToggle: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();

  const sessions = useConversationsStore((s) => s.sessions);
  const fetchSessions = useConversationsStore((s) => s.fetch);
  const resume = useConversationsStore((s) => s.resume);

  // Scope inputs: the listing RPC is agent-scoped for non-admins, so a partner
  // switch changes the result set. `sessionsRevision` bumps once per completed
  // turn, which is what makes a just-started conversation appear here. The main
  // agent is the fallback scope, so a late roster load has to re-trigger the
  // fetch — otherwise a non-admin's first (unscoped) attempt returns empty and
  // nothing would ever ask again.
  const selectedAgentId = useChatStore((s) => s.selectedAgentId);
  const sessionsRevision = useChatStore((s) => s.sessionsRevision);
  const activeSessionId = useChatStore((s) => s.sessionId);
  const mainAgentId = useAgentsStore((s) => s.agents.find((a) => a.role === 'main')?.name ?? null);
  // The dashboard socket carries `chat.sessions.*` — not the chat socket, which
  // only exists once the chat page has connected.
  const connectionState = useConnectionStore((s) => s.state);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    void fetchSessions();
  }, [connectionState, selectedAgentId, mainAgentId, sessionsRevision, fetchSessions]);

  const shown = sessions.slice(0, RAIL_LIMIT);

  if (collapsed) return null;

  const open = async (session: ChatSessionSummary) => {
    // Never navigate on a failed load: landing on /chat with the previous
    // conversation still on screen would read as "this row opened the wrong
    // thread". Say what happened and stay put.
    if (!(await resume(session))) {
      toast.error(intl.formatMessage({ id: 'webchat.history.loadFailed', defaultMessage: '無法載入這個對話' }));
      return;
    }
    navigate('/chat');
  };

  return (
    <SidebarGroup>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={!sectionCollapsed}
        className="flex h-8 w-full items-center px-2 text-xs font-medium text-sidebar-foreground/70 outline-none transition-colors hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
      >
        <span className="flex-1 text-left">{intl.formatMessage({ id: 'navGroup.conversations' })}</span>
        <ChevronDown className={cn('size-3.5 transition-transform', sectionCollapsed && '-rotate-90')} />
      </button>
      {!sectionCollapsed && (
        <SidebarMenu>
          {shown.length === 0 ? (
            <p className="px-2 py-1.5 text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'sidebar.noConversations' })}
            </p>
          ) : (
            shown.map((s) => {
              const channel = sessionChannel(s.session_id);
              const title =
                s.title || intl.formatMessage({ id: 'webchat.history.untitled', defaultMessage: '(Untitled)' });
              return (
                <SidebarMenuItem key={s.session_id}>
                  <button
                    type="button"
                    onClick={() => void open(s)}
                    title={`${title} · ${channel ? `${channel.label} · ` : ''}${timeAgo(s.last_active)}`}
                    className={cn(
                      sidebarMenuButtonVariants({ size: 'sm' }),
                      'px-2 text-left',
                      s.session_id === activeSessionId &&
                        'bg-sidebar-accent font-medium text-sidebar-accent-foreground',
                    )}
                  >
                    <span className="min-w-0 flex-1 truncate">{title}</span>
                    {/* Cross-channel sessions (LINE / Telegram / …) are only
                        visible to admins; the badge says where the thread
                        lives so a Web reply isn't sent into the wrong place. */}
                    {channel && channel.key !== 'webchat' && (
                      <span className="shrink-0 rounded bg-muted px-1 py-px text-[10px] leading-4 text-muted-foreground">
                        {channel.label}
                      </span>
                    )}
                  </button>
                </SidebarMenuItem>
              );
            })
          )}
        </SidebarMenu>
      )}
    </SidebarGroup>
  );
}
