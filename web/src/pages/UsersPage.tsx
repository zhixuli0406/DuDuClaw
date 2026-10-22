import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { api, type UserDetail } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import type { UserRole } from '@/stores/auth-store';
import { ROLE_LEVELS } from '@/lib/roles';
import { OrgChartView } from './users/OrgChartView';
import { channelLabel } from './users/dialog-common';
import { CreateUserDialog, EditUserDialog } from './users/UserFormDialogs';
import { BindAgentDialog, BindChannelIdentityDialog } from './users/AccessDialogs';
import { OffboardDialog, RemoveUserDialog } from './users/LifecycleDialogs';
import { listChannelIdentities, type ChannelIdentity } from '@/lib/channel-identity-api';
import {
  Button,
  Badge,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  ActorAvatar,
  Empty,
  Segmented,
  type BadgeProps,
} from '@/components/mds';
import {
  Users,
  UserPlus,
  Pencil,
  Link2,
  UserX,
  Trash2,
  X,
  Eye,
  MoreHorizontal,
  Loader2,
  AlertTriangle,
  ShieldCheck,
  Plus,
} from 'lucide-react';

type UsersView = 'list' | 'org';

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

const COLUMNS = 'minmax(0,1.4fr) minmax(0,0.8fr) minmax(0,1.5fr) minmax(0,1.5fr) minmax(0,0.7fr) 2.5rem';

export function UsersPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const viewerRole = useAuthStore((s) => s.user?.role);
  // View-as gate mirrors the server: strictly lower rank only.
  const canViewDashboard = (targetRole: string, status: string) =>
    status === 'active' &&
    viewerRole != null &&
    (ROLE_LEVELS[targetRole as UserRole] ?? 99) < ROLE_LEVELS[viewerRole];
  const [users, setUsers] = useState<UserDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [showBind, setShowBind] = useState<string | null>(null);
  const [showEdit, setShowEdit] = useState<UserDetail | null>(null);
  const [showOffboard, setShowOffboard] = useState<UserDetail | null>(null);
  const [showRemove, setShowRemove] = useState<UserDetail | null>(null);
  // WP-B (2026-08-12 IA audit §2-1) — per-user channel identity ("驗證身分",
  // dashboard user ↔ channel DM identity). `'error'` marks a row whose fetch
  // failed; a missing key means "not fetched yet".
  const [identities, setIdentities] = useState<Record<string, ChannelIdentity[] | 'error'>>({});
  const [showIdentityBind, setShowIdentityBind] = useState<string | null>(null);
  const [view, setView] = useState<UsersView>('list');
  const nameOf = (id?: string | null) => (id ? users.find((u) => u.id === id)?.display_name : undefined);

  const fetchUsers = useCallback(async () => {
    try {
      setFetchError(null);
      const { users: data } = await api.users.list();
      setUsers(data);
    } catch (e) {
      setFetchError(e instanceof Error ? e.message : 'Failed to load users');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchUsers();
  }, [fetchUsers]);

  const refreshIdentity = useCallback((userId: string) => {
    listChannelIdentities(userId)
      .then((list) => setIdentities((prev) => ({ ...prev, [userId]: list })))
      .catch(() => setIdentities((prev) => ({ ...prev, [userId]: 'error' })));
  }, []);

  // Fetch each user's channel identities once (skip ids already fetched, so
  // an unrelated `fetchUsers()` re-run after e.g. an edit doesn't re-fire N
  // requests). Small admin rosters only — no pagination on this endpoint yet.
  useEffect(() => {
    const missing = users.map((u) => u.id).filter((id) => !(id in identities));
    missing.forEach(refreshIdentity);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [users]);

  // UI.1 — unbind a single user↔agent binding.
  const handleUnbind = useCallback(async (userId: string, agentName: string) => {
    try {
      await api.users.unbindAgent(userId, agentName);
      toast.success(intl.formatMessage({ id: 'users.unbound' }));
      fetchUsers();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [intl, fetchUsers]);

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Users className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.users' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'nav.users.desc' })}</p>
          </div>
        </div>
        <div className="flex gap-2">
          <Button variant="brand" size="sm" onClick={() => setShowCreate(true)}>
            <UserPlus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'users.create' })}</span>
          </Button>
        </div>
      </div>

      {fetchError && (
        <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-4 py-3 text-sm text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <span>{fetchError}</span>
        </div>
      )}

      {!loading && users.length > 0 && (
        <Segmented<UsersView>
          value={view}
          onValueChange={setView}
          aria-label={intl.formatMessage({ id: 'users.view.label' })}
          options={[
            { value: 'list', label: intl.formatMessage({ id: 'users.view.list' }) },
            { value: 'org', label: intl.formatMessage({ id: 'users.view.org' }) },
          ]}
        />
      )}

      {loading ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="size-6 animate-spin text-muted-foreground" />
        </div>
      ) : users.length === 0 ? (
        <Empty icon={Users} title={intl.formatMessage({ id: 'users.title' })} />
      ) : view === 'org' ? (
        <OrgChartView users={users} onEdit={setShowEdit} />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={COLUMNS}
            className="!h-auto [&>[aria-hidden]]:hidden"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'users.col.name' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'users.col.role' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'users.col.agents' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'users.col.channelIdentity' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'users.col.status' })}</ListGridHeaderCell>
                <ListGridHeaderCell aria-hidden />
              </ListGridHeader>
            }
          >
            {users.map((user) => {
              const role = roleBadgeProps(user.role);
              const status = statusBadgeProps(user.status);
              return (
                <ListGridRow key={user.id} rowSize="lg" className="cursor-default">
                  <ListGridCell className="gap-2">
                    <ActorAvatar actorType="user" name={user.display_name} size="sm" />
                    <div className="min-w-0">
                      <p className="truncate text-sm font-medium text-foreground">{user.display_name}</p>
                      <p className="truncate text-xs text-muted-foreground">{user.email}</p>
                    </div>
                  </ListGridCell>
                  <ListGridCell className="flex-col items-start gap-0.5">
                    <Badge variant={role.variant} className={role.className}>{user.role}</Badge>
                    {user.department && (
                      <span className="truncate text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'users.dept.prefix' })}{user.department}
                      </span>
                    )}
                    {nameOf(user.manager_user_id) && (
                      <span className="truncate text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'users.manager.prefix' })}{nameOf(user.manager_user_id)}
                      </span>
                    )}
                  </ListGridCell>
                  <ListGridCell className="flex-wrap gap-1 py-1.5">
                    {user.bindings.map((b) => (
                      <span
                        key={b.agent_name}
                        className="group inline-flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
                      >
                        <ActorAvatar actorType="agent" name={b.agent_name} size="xs" />
                        {b.agent_name}
                        <span className="text-muted-foreground/70">({b.access_level})</span>
                        <button
                          type="button"
                          data-stop-row-nav
                          onClick={() => handleUnbind(user.id, b.agent_name)}
                          className="rounded-full p-0.5 opacity-0 transition-opacity hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100 pointer-coarse:opacity-100"
                          title={intl.formatMessage({ id: 'users.action.unbind' })}
                          aria-label={`unbind ${b.agent_name}`}
                        >
                          <X className="size-3" />
                        </button>
                      </span>
                    ))}
                    {user.bindings.length === 0 && <span className="text-xs text-muted-foreground">—</span>}
                  </ListGridCell>
                  <ListGridCell className="flex-wrap gap-1 py-1.5">
                    {(() => {
                      const rec = identities[user.id];
                      if (rec === undefined) {
                        return <Loader2 className="size-3.5 animate-spin text-muted-foreground" />;
                      }
                      if (rec === 'error') {
                        return (
                          <span className="text-xs text-destructive">
                            {intl.formatMessage({ id: 'users.channelIdentity.loadError' })}
                          </span>
                        );
                      }
                      return rec.map((ident) => (
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
                      ));
                    })()}
                    <button
                      type="button"
                      data-stop-row-nav
                      onClick={() => setShowIdentityBind(user.id)}
                      className="inline-flex items-center gap-1 rounded-full border border-dashed border-surface-border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:border-brand hover:text-brand"
                      title={intl.formatMessage({ id: 'users.channelIdentity.add' })}
                    >
                      <Plus className="size-3" />
                      {intl.formatMessage({ id: 'users.channelIdentity.add' })}
                    </button>
                  </ListGridCell>
                  <ListGridCell>
                    <Badge variant={status.variant} className={status.className}>{user.status}</Badge>
                  </ListGridCell>
                  <ListGridCell className="justify-end">
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
                        {canViewDashboard(user.role, user.status) && (
                          <DropdownMenuItem onClick={() => navigate(`/?view_as=${encodeURIComponent(user.id)}`)}>
                            <Eye />
                            {intl.formatMessage({ id: 'users.action.viewDashboard' })}
                          </DropdownMenuItem>
                        )}
                        <DropdownMenuItem onClick={() => setShowEdit(user)}>
                          <Pencil />
                          {intl.formatMessage({ id: 'users.action.edit' })}
                        </DropdownMenuItem>
                        <DropdownMenuItem onClick={() => setShowBind(user.id)}>
                          <Link2 />
                          {intl.formatMessage({ id: 'users.action.bind' })}
                        </DropdownMenuItem>
                        {user.status === 'active' && (
                          <DropdownMenuItem onClick={() => setShowOffboard(user)}>
                            <UserX />
                            {intl.formatMessage({ id: 'users.action.offboard' })}
                          </DropdownMenuItem>
                        )}
                        <DropdownMenuItem variant="destructive" onClick={() => setShowRemove(user)}>
                          <Trash2 />
                          {intl.formatMessage({ id: 'users.action.remove' })}
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </ListGridCell>
                </ListGridRow>
              );
            })}
          </ListGridContainer>
        </div>
      )}

      {/* Create User Dialog */}
      {showCreate && (
        <CreateUserDialog
          users={users}
          onClose={() => setShowCreate(false)}
          onCreated={() => {
            setShowCreate(false);
            fetchUsers();
          }}
        />
      )}

      {/* Edit User Dialog */}
      {showEdit && (
        <EditUserDialog
          user={showEdit}
          users={users}
          onClose={() => setShowEdit(null)}
          onUpdated={() => {
            setShowEdit(null);
            fetchUsers();
          }}
        />
      )}

      {/* Bind Agent Dialog */}
      {showBind && (
        <BindAgentDialog
          userId={showBind}
          boundAgents={users.find((u) => u.id === showBind)?.bindings.map((b) => b.agent_name) ?? []}
          onClose={() => setShowBind(null)}
          onBound={() => {
            const boundUserId = showBind;
            setShowBind(null);
            fetchUsers();
            // R-cross-link (§2-1): access authorization and channel-side
            // identity verification are two different settings — point the
            // admin at the one they likely also need, one click away.
            toast.info(intl.formatMessage({ id: 'users.bind.crossLink' }), {
              action: {
                label: intl.formatMessage({ id: 'users.bind.crossLink.action' }),
                onClick: () => setShowIdentityBind(boundUserId),
              },
            });
          }}
        />
      )}

      {/* Verify Channel Identity Dialog (WP-B, §2-1) */}
      {showIdentityBind && (
        <BindChannelIdentityDialog
          userId={showIdentityBind}
          onClose={() => setShowIdentityBind(null)}
          onBound={() => {
            const boundUserId = showIdentityBind;
            setShowIdentityBind(null);
            refreshIdentity(boundUserId);
            toast.success(intl.formatMessage({ id: 'users.channelIdentity.bound' }));
          }}
        />
      )}

      {/* Offboard Dialog */}
      {showOffboard && (
        <OffboardDialog
          user={showOffboard}
          users={users.filter((u) => u.id !== showOffboard.id && u.status === 'active')}
          onClose={() => setShowOffboard(null)}
          onOffboarded={() => {
            setShowOffboard(null);
            fetchUsers();
          }}
        />
      )}

      {/* Remove (delete) Dialog */}
      {showRemove && (
        <RemoveUserDialog
          user={showRemove}
          onClose={() => setShowRemove(null)}
          onRemoved={() => {
            setShowRemove(null);
            fetchUsers();
          }}
        />
      )}
    </div>
  );
}
