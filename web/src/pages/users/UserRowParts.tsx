// The pieces of one Users-page row, shared by the wide table and the narrow
// (phone) card layout so both show the same thing.
import { useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import type { UserDetail } from '@/lib/api';
import { UNBINDABLE_CHANNELS, type ChannelIdentity } from '@/lib/channel-identity-api';
import { formatDateTime } from '@/lib/company-time';
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

export function UserAvatarStatus({ user, agentName }: { user: UserDetail; agentName: (id: string) => string }) {
  const intl = useIntl();
  const label = intl.formatMessage({ id: 'users.avatar.label' });
  return user.avatar_agent_id ? (
    <Badge variant="secondary" className="max-w-full" title={agentName(user.avatar_agent_id)}>
      <ActorAvatar actorType="agent" name={agentName(user.avatar_agent_id)} size="xs" />
      <span className="truncate">{label}: {agentName(user.avatar_agent_id)}</span>
    </Badge>
  ) : <span className="text-xs text-muted-foreground">{label}: {intl.formatMessage({ id: 'users.manager.none' })}</span>;
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

/** Verified channel identities plus the 驗證身分 button. An identity that
 *  comes from the person's own linked Google account is read-only; an
 *  administrator's Google Chat binding can be removed (`onRemove`). */
export function IdentityChips({
  state,
  onAdd,
  onRemove,
}: {
  state: IdentityState;
  onAdd: () => void;
  onRemove?: (ident: ChannelIdentity) => void;
}) {
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
        state.map((ident) => {
          const fromGoogle = ident.source === 'google_account';
          const removable = !fromGoogle && onRemove !== undefined && UNBINDABLE_CHANNELS.includes(ident.channel);
          const label = `${channelLabel(ident.channel)} · ${Array.from(ident.channel_user_id).slice(-4).join('')}`;
          const proof = ident.proof_status ? intl.formatMessage({ id: `users.channelIdentity.${ident.proof_status}` }) : '';
          const title = [label, fromGoogle ? intl.formatMessage({ id: 'users.channelIdentity.fromGoogle' }) : '',
            ident.proven_at ? intl.formatMessage({ id: 'users.channelIdentity.provenAt' }, { time: formatDateTime(ident.proven_at, { locale: intl.locale }) }) : '',
          ].filter(Boolean).join(' — ');
          return (
            <span
              key={`${ident.source ?? 'binding'}:${ident.channel}:${ident.channel_user_id}`}
              className="group inline-flex max-w-full items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
              title={title}
            >
              <ShieldCheck
                className="size-3 shrink-0 text-success"
                aria-label={intl.formatMessage({ id: 'users.channelIdentity.verifiedTooltip' })}
              />
              <span className="truncate">{label}</span>
              {proof && <span className={`shrink-0 border-l border-border pl-1 font-medium ${ident.proof_status === 'self_confirmed' ? 'text-success' : ''}`}>{proof}</span>}
              {fromGoogle && <Link2 className="size-3 shrink-0" aria-label={intl.formatMessage({ id: 'users.channelIdentity.fromGoogle' })} />}
              {removable && (
                <button
                  type="button"
                  data-stop-row-nav
                  onClick={() => onRemove?.(ident)}
                  className="shrink-0 rounded-full p-0.5 opacity-0 transition-opacity hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100 pointer-coarse:opacity-100"
                  title={intl.formatMessage({ id: 'users.channelIdentity.remove' })}
                  aria-label={`${intl.formatMessage({ id: 'users.channelIdentity.remove' })} ${label}`}
                >
                  <X className="size-3" />
                </button>
              )}
            </span>
          );
        })
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
  onInvite,
}: {
  user: UserDetail;
  onEdit: () => void;
  onBind: () => void;
  onOffboard: () => void;
  onRemove: () => void;
  onInvite?: () => void;
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
        {onInvite && viewerRole === 'admin' && user.status === 'active' && <DropdownMenuItem onClick={onInvite}>{intl.formatMessage({ id: 'users.invite.resend' })}</DropdownMenuItem>}
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
