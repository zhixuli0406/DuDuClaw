import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type UserDetail } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { OrgChartView } from './users/OrgChartView';
import { CreateUserDialog, EditUserDialog } from './users/UserFormDialogs';
import { BindAgentDialog, BindChannelIdentityDialog } from './users/AccessDialogs';
import { OffboardDialog, RemoveUserDialog } from './users/LifecycleDialogs';
import {
  BindingChips,
  IdentityChips,
  RoleBadge,
  RoleDetails,
  StatusBadge,
  UserActions,
  useAgentDisplayName,
} from './users/UserRowParts';
import { listChannelIdentities, type ChannelIdentity } from '@/lib/channel-identity-api';
import {
  Button,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  ActorAvatar,
  Empty,
  Segmented,
  useIsMobile,
} from '@/components/mds';
import { Users, UserPlus, Loader2, AlertTriangle } from 'lucide-react';

type UsersView = 'list' | 'org';

const COLUMNS = 'minmax(0,1.4fr) minmax(0,0.8fr) minmax(0,1.5fr) minmax(0,1.5fr) minmax(0,0.7fr) 2.5rem';

export function UsersPage() {
  const intl = useIntl();
  const isMobile = useIsMobile();
  const agentName = useAgentDisplayName();
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
  const actionsFor = (user: UserDetail) => ({
    onEdit: () => setShowEdit(user),
    onBind: () => setShowBind(user.id),
    onOffboard: () => setShowOffboard(user),
    onRemove: () => setShowRemove(user),
  });

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
        isMobile ? (
          // Phones: one stacked card per person (the six-column grid does
          // not fit, and the page must not scroll sideways).
          <ul className="space-y-3">
            {users.map((user) => (
              <li key={user.id} className="space-y-2 rounded-xl border border-surface-border bg-card px-4 py-3">
                <div className="flex items-start gap-2">
                  <ActorAvatar actorType="user" name={user.display_name} size="sm" />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium text-foreground">{user.display_name}</p>
                    <p className="truncate text-xs text-muted-foreground">{user.email}</p>
                  </div>
                  <UserActions user={user} {...actionsFor(user)} />
                </div>
                <div className="flex flex-wrap items-center gap-1.5">
                  <RoleBadge role={user.role} />
                  <StatusBadge status={user.status} />
                </div>
                <div className="flex flex-col gap-0.5">
                  <RoleDetails user={user} managerName={nameOf(user.manager_user_id)} />
                </div>
                <div className="space-y-1">
                  <p className="text-xs font-medium text-muted-foreground">
                    {intl.formatMessage({ id: 'users.col.agents' })}
                  </p>
                  <div className="flex flex-wrap gap-1">
                    <BindingChips user={user} agentName={agentName} onUnbind={(a) => handleUnbind(user.id, a)} />
                  </div>
                </div>
                <div className="space-y-1">
                  <p className="text-xs font-medium text-muted-foreground">
                    {intl.formatMessage({ id: 'users.col.channelIdentity' })}
                  </p>
                  <div className="flex flex-wrap gap-1">
                    <IdentityChips state={identities[user.id]} onAdd={() => setShowIdentityBind(user.id)} />
                  </div>
                </div>
              </li>
            ))}
          </ul>
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
            {users.map((user) => (
              <ListGridRow key={user.id} rowSize="lg" className="cursor-default">
                <ListGridCell className="gap-2">
                  <ActorAvatar actorType="user" name={user.display_name} size="sm" />
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium text-foreground">{user.display_name}</p>
                    <p className="truncate text-xs text-muted-foreground">{user.email}</p>
                  </div>
                </ListGridCell>
                <ListGridCell className="flex-col items-start gap-0.5">
                  <RoleBadge role={user.role} />
                  <RoleDetails user={user} managerName={nameOf(user.manager_user_id)} />
                </ListGridCell>
                <ListGridCell className="flex-wrap gap-1 py-1.5">
                  <BindingChips user={user} agentName={agentName} onUnbind={(a) => handleUnbind(user.id, a)} />
                </ListGridCell>
                <ListGridCell className="flex-wrap gap-1 py-1.5">
                  <IdentityChips state={identities[user.id]} onAdd={() => setShowIdentityBind(user.id)} />
                </ListGridCell>
                <ListGridCell>
                  <StatusBadge status={user.status} />
                </ListGridCell>
                <ListGridCell className="justify-end">
                  <UserActions user={user} {...actionsFor(user)} />
                </ListGridCell>
              </ListGridRow>
            ))}
          </ListGridContainer>
        </div>
        )
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
