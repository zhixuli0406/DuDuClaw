import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import type { UserDetail } from '@/lib/api';
import { buildOrgTree, type OrgNode } from '@/lib/org-tree';
import { ActorAvatar, Badge } from '@/components/mds';
import { useAgentDisplayName } from './UserRowParts';

/**
 * 組織圖: the Users page's reporting lines, drawn from `manager_user_id`.
 * Each person shows their role and the AI employees bound to them; people
 * with no manager and no reports are listed under 「未指定主管」.
 */
export function OrgChartView({
  users,
  onEdit,
}: {
  users: UserDetail[];
  onEdit: (user: UserDetail) => void;
}) {
  const intl = useIntl();
  const tree = useMemo(() => buildOrgTree(users), [users]);

  return (
    <div className="space-y-4" data-testid="org-chart">
      {tree.roots.length === 0 && (
        <p className="rounded-xl border border-dashed border-surface-border px-4 py-6 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'users.org.empty' })}
        </p>
      )}
      {tree.roots.map((root) => (
        <section
          key={root.person.id}
          className="rounded-xl border border-surface-border bg-card px-4 py-3"
        >
          <ul role="tree" aria-label={root.person.display_name}>
            <OrgBranch node={root} onEdit={onEdit} />
          </ul>
        </section>
      ))}
      {tree.unassigned.length > 0 && (
        <section className="rounded-xl border border-surface-border bg-card px-4 py-3">
          <h2 className="mb-2 text-xs font-medium text-muted-foreground">
            {intl.formatMessage({ id: 'users.org.unassigned' })}
          </h2>
          <ul className="space-y-1">
            {tree.unassigned.map((u) => (
              <li key={u.id}>
                <PersonRow user={u} onEdit={onEdit} />
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

function OrgBranch({ node, onEdit }: { node: OrgNode<UserDetail>; onEdit: (u: UserDetail) => void }) {
  return (
    <li role="treeitem" aria-expanded={node.reports.length > 0 ? true : undefined} aria-selected={false}>
      <PersonRow user={node.person} onEdit={onEdit} />
      {node.reports.length > 0 && (
        <ul role="group" className="ml-4 border-l border-surface-border pl-4">
          {node.reports.map((r) => (
            <OrgBranch key={r.person.id} node={r} onEdit={onEdit} />
          ))}
        </ul>
      )}
    </li>
  );
}

function PersonRow({ user, onEdit }: { user: UserDetail; onEdit: (u: UserDetail) => void }) {
  const intl = useIntl();
  const agentName = useAgentDisplayName();
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
      <button
        type="button"
        onClick={() => onEdit(user)}
        className="flex min-w-0 items-center gap-2 rounded-md text-left outline-none transition-colors hover:text-brand focus-visible:ring-3 focus-visible:ring-ring/50"
        title={intl.formatMessage({ id: 'users.action.edit' })}
      >
        <ActorAvatar actorType="user" name={user.display_name} size="sm" />
        <span className="truncate text-sm font-medium">{user.display_name}</span>
      </button>
      <Badge variant={user.role === 'employee' ? 'outline' : 'secondary'} className="shrink-0 whitespace-nowrap">
        {intl.formatMessage({ id: `users.org.role.${user.role}` })}
      </Badge>
      {user.status === 'suspended' && (
        <Badge variant="secondary" className="shrink-0 whitespace-nowrap bg-warning/15 text-warning">
          {intl.formatMessage({ id: 'users.org.suspended' })}
        </Badge>
      )}
      {user.department && (
        <span className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'users.dept.prefix' })}
          {user.department}
        </span>
      )}
      <div className="flex flex-wrap gap-1">
        {user.bindings.map((b) => (
          <span
            key={b.agent_name}
            className="inline-flex max-w-full items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
          >
            <ActorAvatar actorType="agent" name={agentName(b.agent_name)} size="xs" />
            <span className="truncate">{agentName(b.agent_name)}</span>
          </span>
        ))}
      </div>
    </div>
  );
}
