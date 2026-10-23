// The pieces of one Users-page row, shared by the wide table and the narrow
// (phone) card layout so both show the same thing.
import { useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import type { UserDetail } from '@/lib/api';
import type { ChannelIdentity } from '@/lib/channel-identity-api';
import { ROLE_LEVELS } from '@/lib/roles';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore, type UserRole } from '@/stores/auth-store';
import {
  ActorAvatar,
  Badge,
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  type BadgeProps,
} from '@/components/mds';
import { Eye, Link2, Loader2, MoreHorizontal, Pencil, Plus, ShieldCheck, Trash2, UserX, X } from 'lucide-react';
import { channelLabel } from './dialog-common';

/** `channel_identities` fetch state per user: list, `'error'`, or absent. */
export type IdentityState = ChannelIdentity[] | 'error' | undefined;

/** AI employee id → display name, from the roster the dashboard already
 *  keeps (an admin's roster lists every employee). Unknown ids fall back to
 *  the id itself. */
export function useAgentDisplayName(): (id: string) => string {
  const agents = useAgentsStore((s) => s.agents);
  const loaded = useAgentsStore((s) => s.loaded);
  const loading = useAgentsStore((s) => s.loading);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  useEffect(() => {
    if (!loaded && !loading) void fetchAgents();
  }, [loaded, loading, fetchAgents]);
  const names = useMemo(
    () => new Map(agents.map((a) => [a.name, a.display_name?.trim() || a.name])),
    [agents],
  );
  return (id) => names.get(id) ?? id;
}

function roleBadgeProps(role: string): { variant: BadgeProps['variant']; className?: string } {
  if (role === 'admin') return { variant: 'destructive' };
  if (role === 'manager') return { variant: 'secondary' };
  return { variant: 'outline' };
}

function statusBadgeProps(status: string): { variant: BadgeProps['variant']; className?: string } {
  if (status === 'active') return { variant: 'secondary', className: 'bg-success/15 text-success' };
  if (status === 'suspended') return { variant: 'secondary', className: 'bg-warning/15 text-warning' };
  return { variant: 'outline' };
}

const KNOWN_ROLES = new Set(['admin', 'manager', 'employee']);
const KNOWN_STATUSES = new Set(['active', 'suspended', 'offboarded']);
const KNOWN_LEVELS = new Set(['owner', 'operator', 'viewer']);

export function RoleBadge({ role }: { role: string }) {
  const intl = useIntl();
  const p = roleBadgeProps(role);
  return (
    <Badge variant={p.variant} className={`shrink-0 whitespace-nowrap ${p.className ?? ''}`}>
      {KNOWN_ROLES.has(role) ? intl.formatMessage({ id: `users.org.role.${role}` }) : role}
    </Badge>
  );
}

export function StatusBadge({ status }: { status: string }) {
  const intl = useIntl();
  const p = statusBadgeProps(status);
  return (
    <Badge variant={p.variant} className={`shrink-0 whitespace-nowrap ${p.className ?? ''}`}>
      {KNOWN_STATUSES.has(status) ? intl.formatMessage({ id: `users.status.${status}` }) : status}
    </Badge>
  );
}

/** Department and direct-manager lines under the role. */
export function RoleDetails({ user, managerName }: { user: UserDetail; managerName?: string }) {
  const intl = useIntl();
  return (
    <>
      {user.department && (
        <span className="truncate text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'users.dept.prefix' })}
          {user.department}
        </span>
      )}
      {managerName && (
        <span className="truncate text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'users.manager.prefix' })}
          {managerName}
        </span>
      )}
    </>
  );
}

/** The AI employees bound to `user`, by display name, each with an unbind
 *  button. */
export function BindingChips({
  user,
  agentName,
  onUnbind,
}: {
  user: UserDetail;
  agentName: (id: string) => string;
  onUnbind: (agentId: string) => void;
}) {
  const intl = useIntl();
  if (user.bindings.length === 0) return <span className="text-xs text-muted-foreground">—</span>;
  return (
    <>
      {user.bindings.map((b) => {
        const name = agentName(b.agent_name);
        return (
          <span
            key={b.agent_name}
            className="group inline-flex max-w-full items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
            title={name === b.agent_name ? name : `${name} · ${b.agent_name}`}
          >
            <ActorAvatar actorType="agent" name={name} size="xs" />
            <span className="truncate">{name}</span>
            <span className="shrink-0 text-muted-foreground/70">
              (
              {KNOWN_LEVELS.has(b.access_level)
                ? intl.formatMessage({ id: `users.access.${b.access_level}` })
                : b.access_level}
              )
            </span>
            <button
              type="button"
              data-stop-row-nav
              onClick={() => onUnbind(b.agent_name)}
              className="shrink-0 rounded-full p-0.5 opacity-0 transition-opacity hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100 pointer-coarse:opacity-100"
              title={intl.formatMessage({ id: 'users.action.unbind' })}
              aria-label={`unbind ${b.agent_name}`}
            >
              <X className="size-3" />
            </button>
          </span>
        );
      })}
    </>
  );
}

/** Verified channel identities plus the 驗證身分 button. */
export function IdentityChips({ state, onAdd }: { state: IdentityState; onAdd: () => void }) {
  const intl = useIntl();
  return (
    <>
      {state === undefined ? (
        <Loader2 className="size-3.5 animate-spin text-muted-foreground" />
      ) : state === 'error' ? (
        <span className="text-xs text-destructive">
          {intl.formatMessage({ id: 'users.channelIdentity.loadError' })}
        </span>
      ) : (
        state.map((ident) => (
          <span
            key={`${ident.channel}:${ident.channel_user_id}`}
            className="inline-flex max-w-40 items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
            title={`${channelLabel(ident.channel)} · ${ident.channel_user_id}`}
          >
            <ShieldCheck
              className="size-3 shrink-0 text-success"
              aria-label={intl.formatMessage({ id: 'users.channelIdentity.verifiedTooltip' })}
            />
            <span className="truncate">
              {channelLabel(ident.channel)} · {ident.channel_user_id}
            </span>
          </span>
        ))
      )}
      <button
        type="button"
        data-stop-row-nav
        onClick={onAdd}
        className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded-full border border-dashed border-surface-border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:border-brand hover:text-brand"
        title={intl.formatMessage({ id: 'users.channelIdentity.add' })}
      >
        <Plus className="size-3" />
        {intl.formatMessage({ id: 'users.channelIdentity.add' })}
      </button>
    </>
  );
}

/** The row's ⋯ menu. */
export function UserActions({
  user,
  onEdit,
  onBind,
  onOffboard,
  onRemove,
}: {
  user: UserDetail;
  onEdit: () => void;
  onBind: () => void;
  onOffboard: () => void;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  const viewerRole = useAuthStore((s) => s.user?.role);
  // View-as gate mirrors the server: strictly lower rank only.
  const canViewDashboard =
    user.status === 'active' &&
    viewerRole != null &&
    (ROLE_LEVELS[user.role as UserRole] ?? 99) < ROLE_LEVELS[viewerRole];
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={intl.formatMessage({ id: 'common.more' })}
            data-stop-row-nav
          />
        }
      >
        <MoreHorizontal />
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        {canViewDashboard && (
          <DropdownMenuItem onClick={() => navigate(`/?view_as=${encodeURIComponent(user.id)}`)}>
            <Eye />
            {intl.formatMessage({ id: 'users.action.viewDashboard' })}
          </DropdownMenuItem>
        )}
        <DropdownMenuItem onClick={onEdit}>
          <Pencil />
          {intl.formatMessage({ id: 'users.action.edit' })}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onBind}>
          <Link2 />
          {intl.formatMessage({ id: 'users.action.bind' })}
        </DropdownMenuItem>
        {user.status === 'active' && (
          <DropdownMenuItem onClick={onOffboard}>
            <UserX />
            {intl.formatMessage({ id: 'users.action.offboard' })}
          </DropdownMenuItem>
        )}
        <DropdownMenuItem variant="destructive" onClick={onRemove}>
          <Trash2 />
          {intl.formatMessage({ id: 'users.action.remove' })}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
