import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { AlertTriangle, Eye, Loader2, MessageCircle, Send, UsersRound } from 'lucide-react';
import { api, type MyTeam, type TeamAgent, type TeamMember } from '@/lib/api';
import { formatError } from '@/lib/toast';
import { MAX_ORG_DEPTH } from '@/lib/org-tree';
import { ActorAvatar, Badge, Button, Empty } from '@/components/mds';
import { TeamAssignDialog } from './team/TeamAssignDialog';

interface TeamNode {
  member: TeamMember;
  reports: TeamNode[];
}

/** Nest the flat `users.my_team` rows under their managers. The reply is
 *  already a bounded tree; the visited set and depth cap only make sure a
 *  malformed one can never loop, and a row whose manager is missing still
 *  shows at the top level. */
export function nestTeam(members: TeamMember[], managerId: string | null): TeamNode[] {
  const ids = new Set(members.map((m) => m.id));
  const byManager = new Map<string, TeamMember[]>();
  for (const m of members) {
    const parent = m.manager_user_id && ids.has(m.manager_user_id) ? m.manager_user_id : '';
    byManager.set(parent, [...(byManager.get(parent) ?? []), m]);
  }
  const placed = new Set<string>();
  const grow = (m: TeamMember, depth: number): TeamNode => {
    placed.add(m.id);
    const reports =
      depth >= MAX_ORG_DEPTH
        ? []
        : (byManager.get(m.id) ?? []).filter((r) => !placed.has(r.id)).map((r) => grow(r, depth + 1));
    return { member: m, reports };
  };
  const top = (byManager.get('') ?? []).filter((m) => m.id !== managerId);
  const roots = top.map((m) => grow(m, 1));
  // Anything still unplaced (a cycle in malformed data) is shown flat.
  const rest = members.filter((m) => !placed.has(m.id)).map((m) => ({ member: m, reports: [] }));
  return [...roots, ...rest];
}

/**
 * 我的團隊 (`/team`, Enterprise, manager+). The people who report to the
 * viewer, directly or further down, and the AI employees the viewer can
 * talk to or hand work to through each of them. The gateway computes the
 * list (`users.my_team`) with the same rules that decide every call, so
 * what is offered here is exactly what will be allowed.
 */
export function TeamPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const [team, setTeam] = useState<MyTeam | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [assigning, setAssigning] = useState<{ agent: TeamAgent; report: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setError(null);
      setTeam(await api.users.myTeam());
    } catch (e) {
      // Drop what was shown before: a refusal after a role, edition or
      // reporting-line change must not leave the old names on screen.
      setTeam(null);
      setError(formatError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const tree = useMemo(
    () => (team ? nestTeam(team.members, team.manager?.id ?? null) : []),
    [team],
  );
  const agentCount = useMemo(
    () => new Set(team?.members.flatMap((m) => m.agents.map((a) => a.name)) ?? []).size,
    [team],
  );

  const openChat = (agent: TeamAgent) => {
    navigate('/chat', { state: { draft: `${agent.call_text} ` } });
  };

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      <div className="flex min-w-0 flex-wrap items-center gap-2">
        <UsersRound className="size-5 text-muted-foreground" />
        <div>
          <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.myTeam' })}</h1>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'team.subtitle' })}</p>
        </div>
        {team?.members.some(m => m.agents.some(a => a.history_readable === true)) && <Button variant="outline" size="sm" className="ml-auto" onClick={() => navigate('/team/conversations')}><Eye />{intl.formatMessage({ id: 'teamHistory.title' })}</Button>}
      </div>

      {error && (
        <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-4 py-3 text-sm text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {loading ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="size-6 animate-spin text-muted-foreground" />
        </div>
      ) : team && tree.length === 0 ? (
        <Empty
          icon={UsersRound}
          title={intl.formatMessage({ id: 'team.empty.title' })}
          description={intl.formatMessage({ id: 'team.empty.desc' })}
        />
      ) : team ? (
        <>
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage(
              { id: 'team.summary' },
              { people: team.members.length, agents: agentCount },
            )}
          </p>
          <ul className="space-y-3">
            {tree.map((node) => (
              <li key={node.member.id} className="rounded-xl border border-surface-border bg-card px-4 py-3">
                <TeamBranch
                  node={node}
                  onChat={openChat}
                  onAssign={(agent, report) => setAssigning({ agent, report })}
                  onHistory={(agent, report) => navigate(`/team/conversations?${new URLSearchParams({ agent: agent.name, report })}`)}
                />
              </li>
            ))}
          </ul>
        </>
      ) : null}

      {assigning && (
        <TeamAssignDialog
          agent={assigning.agent}
          reportName={assigning.report}
          onClose={() => {
            setAssigning(null);
            void load();
          }}
        />
      )}
    </div>
  );
}

function TeamBranch({
  node,
  onChat,
  onAssign,
  onHistory,
}: {
  node: TeamNode;
  onChat: (agent: TeamAgent) => void;
  onAssign: (agent: TeamAgent, report: string) => void;
  onHistory: (agent: TeamAgent, report: string) => void;
}) {
  return (
    <div>
      <MemberCard member={node.member} onChat={onChat} onAssign={onAssign} onHistory={onHistory} />
      {node.reports.length > 0 && (
        <ul className="mt-2 ml-4 space-y-2 border-l border-surface-border pl-4">
          {node.reports.map((r) => (
            <li key={r.member.id}>
              <TeamBranch node={r} onChat={onChat} onAssign={onAssign} onHistory={onHistory} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function MemberCard({
  member,
  onChat,
  onAssign,
  onHistory,
}: {
  member: TeamMember;
  onChat: (agent: TeamAgent) => void;
  onAssign: (agent: TeamAgent, report: string) => void;
  onHistory: (agent: TeamAgent, report: string) => void;
}) {
  const intl = useIntl();
  return (
    <section aria-label={member.display_name} className="space-y-2 py-1">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <ActorAvatar actorType="user" name={member.display_name} size="sm" />
        <span className="min-w-0 truncate text-sm font-medium text-foreground">{member.display_name}</span>
        <Badge variant={member.role === 'employee' ? 'outline' : 'secondary'} className="shrink-0 whitespace-nowrap">
          {intl.formatMessage({ id: `users.org.role.${member.role}` })}
        </Badge>
        {member.status === 'suspended' && (
          <Badge variant="secondary" className="shrink-0 whitespace-nowrap bg-warning/15 text-warning">
            {intl.formatMessage({ id: 'users.org.suspended' })}
          </Badge>
        )}
        {member.department && (
          <span className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'users.dept.prefix' })}
            {member.department}
          </span>
        )}
      </div>
      {member.agents.length === 0 ? (
        <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'team.noAgents' })}</p>
      ) : (
        <ul className="space-y-1.5">
          {member.agents.map((agent) => (
            <li key={agent.name}>
              <AgentRow
                agent={agent}
                onChat={() => onChat(agent)}
                onAssign={() => onAssign(agent, member.display_name)}
                onHistory={() => onHistory(agent, member.id)}
              />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function AgentRow({
  agent,
  onChat,
  onAssign,
  onHistory,
}: {
  agent: TeamAgent;
  onChat: () => void;
  onAssign: () => void;
  onHistory: () => void;
}) {
  const intl = useIntl();
  return (
    // Stacked on phones (name, then tags, then buttons); one line from `sm`.
    <div className="flex flex-col gap-2 rounded-lg bg-muted/50 px-3 py-2 sm:flex-row sm:items-center sm:gap-3">
      <div className="flex min-w-0 flex-1 flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <ActorAvatar actorType="agent" name={agent.display_name} size="xs" />
          <span className="truncate text-sm text-foreground">{agent.display_name}</span>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge variant="outline" className="shrink-0 whitespace-nowrap">
            {intl.formatMessage({ id: agent.assignable ? 'team.level.assign' : 'team.level.talk' })}
          </Badge>
          {!agent.operational && (
            <Badge variant="secondary" className="shrink-0 whitespace-nowrap bg-warning/15 text-warning">
              {intl.formatMessage({ id: 'team.agent.unavailable' })}
            </Badge>
          )}
          {agent.active_tasks !== null && (
            <span className="whitespace-nowrap text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'team.activeTasks' }, { count: agent.active_tasks })}
            </span>
          )}
        </div>
      </div>
      <div className="flex shrink-0 gap-2">
        {agent.history_readable === true && <Button variant="outline" size="sm" onClick={onHistory}><Eye />{intl.formatMessage({ id: 'teamHistory.view' })}</Button>}
        <Button variant="outline" size="sm" onClick={onChat} disabled={!agent.operational}>
          <MessageCircle />
          {intl.formatMessage({ id: 'team.action.chat' })}
        </Button>
        <Button variant="brand" size="sm" onClick={onAssign} disabled={!agent.assignable}>
          <Send />
          {intl.formatMessage({ id: 'team.action.assign' })}
        </Button>
      </div>
    </div>
  );
}
