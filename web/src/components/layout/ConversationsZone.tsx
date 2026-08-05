import { useEffect } from 'react';
import { NavLink, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { ArrowRight, ChevronDown } from 'lucide-react';
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
import { conversationsEntry } from './nav-model';

/**
 * How many past conversations the rail lists. Cut from 15 to 5 on 2026-08-04
 * (D17): fifteen titles pushed every other navigation group below the fold, and
 * "what was I just doing" only ever needs the last handful. Everything older
 * lives on the full 對話紀錄 page, which the 查看全部 row links to.
 */
const RAIL_LIMIT = 5;

/**
 * 對話紀錄 — the sidebar conversation-history zone (2026-07-30 client feedback).
 *
 * Sits at the tail of the working area, directly above 設定 (moved there on
 * 2026-08-04, WP18-B — it used to sit under 新對話, where it split the
 * client-annotated primary order). Rows resume the stored session in place; the
 * chat page's left column keeps the full list (all employees, all channels), so
 * this zone is a shortcut, never the only way in.
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
  const listStatus = useConversationsStore((s) => s.status);
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
            listStatus === 'error' ? (
              // A failed listing must not read as "no conversations" — offer a
              // retry instead of a lie.
              <button
                type="button"
                onClick={() => void fetchSessions()}
                className="px-2 py-1.5 text-left text-xs text-muted-foreground underline-offset-2 hover:underline"
              >
                {intl.formatMessage({ id: 'sidebar.conversationsError' })}
              </button>
            ) : (
              <p className="px-2 py-1.5 text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'sidebar.noConversations' })}
              </p>
            )
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
          {/* 查看全部 — the rest of the history lives on its own page (D17).
              Pointless when there is nothing to see yet, so it waits for the
              first conversation. */}
          {shown.length > 0 && (
          <SidebarMenuItem>
            <NavLink
              to={conversationsEntry.to}
              data-tour={`nav:${conversationsEntry.to}`}
              className={cn(sidebarMenuButtonVariants({ size: 'sm' }), 'px-2 text-brand hover:text-brand')}
            >
              <span className="flex-1 truncate">{intl.formatMessage({ id: 'sidebar.allConversations' })}</span>
              <ArrowRight className="size-3.5" />
            </NavLink>
          </SidebarMenuItem>
          )}
        </SidebarMenu>
      )}
    </SidebarGroup>
  );
}
