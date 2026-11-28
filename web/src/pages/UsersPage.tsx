import { AgentSeatSummary } from '@/components/shared/AgentSeatSummary';
import { useAdminStepUp } from '@/components/step-up/useAdminStepUp';
import { ConfirmDialog, RiskConfirmDialog } from '@/components/settings/controls';
import { memberSignin, type OdooMemberCredential } from '@/lib/member-signin';
import { settingsRefusal } from '@/lib/settings-refusal';
import { AdminMfa } from '@/components/mfa/AdminMfa';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type UserDetail, type UserInviteState, type PrimaryAgentRepairPreview, type AvatarConflict } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { dashboardBaseUrl } from '@/lib/dashboard-base';
import { InviteEmailPreview, InviteLinkDialog, InviteStatusChip } from './users/InviteControls';
import { useAuthStore } from '@/stores/auth-store';
import { AddMembersPage } from './users/AddMembersPage';
import { SigninRequestsPanel, useSigninRequests } from './users/SigninRequestsPanel';
import { OrgChartView } from './users/OrgChartView';
import { OrgHistorySettings } from './users/OrgHistorySettings';
import { EditUserDialog } from './users/UserFormDialogs';
import { CreateAvatarDialog } from './users/CreateAvatarDialog';
import { useAgentsStore } from '@/stores/agents-store';
import { BindAgentDialog, BindChannelIdentityDialog } from './users/AccessDialogs';
import { OffboardDialog, RemoveUserDialog } from './users/LifecycleDialogs';
import {
  MemberSigninBadges,
  BindingChips,
  IdentityChips,
  RoleBadge,
  RoleDetails,
  StatusBadge,
  UserActions,
  UserAvatarStatus,
  useAgentDisplayName,
} from './users/UserRowParts';
import { listChannelIdentities, unbindChannelIdentity, type ChannelIdentity } from '@/lib/channel-identity-api';
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter,
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
  useErrorMessage,
} from '@/components/mds';
import { Users, UserPlus, Loader2, AlertTriangle } from 'lucide-react';

type UsersView = 'list' | 'org' | 'pending';

const COLUMNS = 'minmax(0,1.4fr) minmax(0,0.8fr) minmax(0,1.5fr) minmax(0,1.5fr) minmax(0,0.7fr) 2.5rem';

export function UsersPage() {
  const intl = useIntl();
  const isMobile = useIsMobile();
  const errorText = useErrorMessage();
  const stepUp = useAdminStepUp();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `users.memberSignin.${key}` }, values);
  const [credentials, setCredentials] = useState<Record<string, OdooMemberCredential>>({});
  const [selected, setSelected] = useState<string[]>([]);
  const [batchOpen, setBatchOpen] = useState(false);
  const [memberConfirm, setMemberConfirm] = useState<{ user: UserDetail; action: 'unlink' | 'revoke' } | null>(null);
  const [memberError, setMemberError] = useState<string | null>(null);
  const [memberBusy, setMemberBusy] = useState(false);
  const memberPending = useRef(false);
  const agentName = useAgentDisplayName();
  const isAdmin = useAuthStore(s => s.user?.role === 'admin');
  const signin = useSigninRequests(isAdmin);
  const [inviteStates, setInviteStates] = useState<Record<string, UserInviteState>>({});
  const [inviteLink, setInviteLink] = useState<string | null>(null);
  const [inviteUser, setInviteUser] = useState<UserDetail | null>(null);
  const [inviting, setInviting] = useState(false);
  const [repairPreview, setRepairPreview] = useState<PrimaryAgentRepairPreview | null>(null);
  const [repairCandidates, setRepairCandidates] = useState<PrimaryAgentRepairPreview | null>(null);
  const [avatarConflicts, setAvatarConflicts] = useState<AvatarConflict[] | null>([]);
  const [repairing, setRepairing] = useState(false);
  const [users, setUsers] = useState<UserDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [showBind, setShowBind] = useState<string | null>(null);
  const [showEdit, setShowEdit] = useState<UserDetail | null>(null);
  const [createAvatarFor, setCreateAvatarFor] = useState<UserDetail | null>(null);
  const [showOffboard, setShowOffboard] = useState<UserDetail | null>(null);
  const [showRemove, setShowRemove] = useState<UserDetail | null>(null);
  // WP-B (2026-08-12 IA audit §2-1) — per-user channel identity ("驗證身分",
  // dashboard user ↔ channel DM identity). `'error'` marks a row whose fetch
  // failed; a missing key means "not fetched yet".
  const [identities, setIdentities] = useState<Record<string, ChannelIdentity[] | 'error'>>({});
  const [showIdentityBind, setShowIdentityBind] = useState<string | null>(null);
  const [view, setView] = useState<UsersView>('list');
  const currentView = view === 'pending' && !signin.available ? 'list' : view;
  const nameOf = (id?: string | null) => (id ? users.find((u) => u.id === id)?.display_name : undefined);
  const avatarAction = (user: UserDetail) => {
    if (!isAdmin || user.status !== 'active') return null;
    const avatar = user.signin_avatar;
    if (avatar?.state === 'waiting_admin') {
      if (avatar.reason === 'protected_mode') return <Button variant="outline" size="sm" onClick={() => setCreateAvatarFor(user)}>{t('createForMember')}</Button>;
      if (avatar.retry_allowed) return <Button variant="outline" size="sm" disabled={memberBusy} onClick={() => void memberAction(() => memberSignin.retryAvatar(user.id))}>{t('retry')}</Button>;
      return null;
    }
    if (avatar && avatar.state !== 'done') return null;
    return (user.role === 'employee' || user.role === 'manager') && !user.avatar_agent_id ? (
      <Button variant="outline" size="sm" onClick={() => setCreateAvatarFor(user)}>
        {intl.formatMessage({ id: 'users.avatar.create' })}
      </Button>
    ) : null;
  };
  const selectionFor = (user: UserDetail) => {
    if (!isAdmin) return null;
    const why = user.google_signin_linked ? t('selectionLinked') : user.status !== 'active' ? t('selectionInactive') : undefined;
    return <span title={why} className="inline-flex p-1">
      <input type="checkbox" aria-label={t('selectMember', { name: user.display_name })} title={why}
        disabled={!!why || memberBusy || batchOpen} checked={selected.includes(user.id)}
        onChange={event => {
          const checked = event.currentTarget.checked;
          if (checked && selected.length >= 100) { setMemberError(t('selectionLimit')); return; }
          setMemberError(null);
          setSelected(previous => checked ? [...previous, user.id] : previous.filter(id => id !== user.id));
        }} />
    </span>;
  };
  const actionsFor = (user: UserDetail) => ({
    onEdit: () => setShowEdit(user),
    ...(isAdmin && user.google_signin_linked ? { onUnlinkGoogle: () => { setMemberError(null); setMemberConfirm({ user, action: 'unlink' }); } } : {}),
    ...(isAdmin && credentials[user.id] ? { onRevokeOdoo: () => { setMemberError(null); setMemberConfirm({ user, action: 'revoke' }); } } : {}),
    ...(isAdmin && user.status === 'active' && (user.invite_eligible === true || inviteStates[user.id] === 'pending' || inviteStates[user.id] === 'expired') ? { onInvite: () => setInviteUser(user) } : {}),
    onBind: () => setShowBind(user.id),
    onOffboard: () => setShowOffboard(user),
    onRemove: () => setShowRemove(user),
  });

  const fetchUsers = useCallback(async () => {
    try {
      setFetchError(null);
      const { users: data } = await api.users.list();
      setUsers(data);
      setSelected(previous => previous.filter(id => data.some(user => user.id === id && user.status === 'active' && !user.google_signin_linked)));
      if (isAdmin) {
        try {
          const result = await memberSignin.credentials();
          setCredentials(Object.fromEntries((result.credentials ?? []).map(credential => [credential.user_id, credential])));
        } catch { setCredentials({}); }
        try { const result = await api.users.avatarConflicts(); setAvatarConflicts(result.conflicts ?? []); }
        catch { setAvatarConflicts(null); }
        try {
          const preview = await api.users.repairPrimaryAgents();
          setRepairCandidates(Array.isArray(preview.members) && typeof preview.preview_digest === 'string' ? preview : null);
        }
        catch { setRepairCandidates(null); }
        try { const result = await api.users.inviteStatus(); setInviteStates(result.statuses ?? {}); }
        catch { setInviteStates({}); }
      }
    } catch (e) {
      setFetchError(e instanceof Error ? e.message : 'Failed to load users');
    } finally {
      setLoading(false);
    }
  }, [isAdmin]);

  const memberAction = async (action: () => Promise<unknown>) => {
    if (!isAdmin || memberPending.current) return;
    memberPending.current = true;
    setMemberBusy(true);
    setMemberError(null);
    try {
      await action();
      setMemberConfirm(null);
      await fetchUsers();
    } catch (err) {
      if (err && typeof err === 'object' && 'code' in err && err.code === 'signin_step_up_required') stepUp.invalidate();
      setMemberError(settingsRefusal(err, errorText(err)));
    } finally { memberPending.current = false; setMemberBusy(false); }
  };
  const attestSelected = () => {
    const ids = [...selected];
    if (!ids.length || ids.length > 100 || memberPending.current) return;
    setBatchOpen(false);
    void memberAction(async () => {
      const token = await stepUp.ensure(t('batchAction'));
      if (!token) return;
      await memberSignin.attest(ids, token);
      setSelected([]);
    });
  };

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

  // RM-152 — remove an administrator's Google Chat binding.
  const handleIdentityRemove = useCallback(async (userId: string, ident: ChannelIdentity) => {
    try {
      await unbindChannelIdentity(userId, ident.channel, ident.channel_user_id);
      toast.success(intl.formatMessage({ id: 'users.channelIdentity.removed' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      refreshIdentity(userId);
    }
  }, [intl, refreshIdentity]);

  const missingPrimaryCount = repairCandidates?.members.length ?? 0;
  const repairPrimary = async (confirm: boolean) => {
    if (repairing || (confirm && !repairPreview)) return;
    setRepairing(true);
    try {
      const result = await api.users.repairPrimaryAgents(confirm ? repairPreview!.preview_digest : undefined);
      if (confirm) { setRepairPreview(null); await fetchUsers(); }
      else { setRepairPreview(result); setRepairCandidates(result); }
    } catch (e) {
      setRepairPreview(null);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally { setRepairing(false); }
  };

  const sendInvite = async () => {
    if (!inviteUser || inviting) return;
    setInviting(true);
    try {
      const result = await api.users.invite(inviteUser.id, dashboardBaseUrl());
      toast[result.sent ? 'success' : 'info'](intl.formatMessage({ id: result.sent ? 'users.invite.sent' : 'users.invite.notSent' }));
      setInviteUser(null);
      if (!result.sent && result.link) setInviteLink(result.link);
      void fetchUsers();
    } catch { toast.error(intl.formatMessage({ id: 'users.invite.failed' })); }
    finally { setInviting(false); }
  };

  if (showCreate && isAdmin) return <AddMembersPage onBack={() => setShowCreate(false)} onCreated={() => void fetchUsers()} />;

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
          <Button variant="brand" size="sm" disabled={!isAdmin} onClick={() => setShowCreate(true)}>
            <UserPlus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'members.title' })}</span>
          </Button>
        </div>
      </div>

      {isAdmin && <AgentSeatSummary refreshKey={users} />}

      {isAdmin && (missingPrimaryCount > 0 || (repairCandidates?.excluded_personal_count ?? 0) > 0) && <div className="flex flex-wrap items-center gap-3 rounded-lg border border-surface-border bg-card px-4 py-3 text-sm">
        <span>{intl.formatMessage({ id: 'users.primary.missing' }, { count: missingPrimaryCount })}</span>
        <Button variant="outline" disabled={repairing} onClick={() => void repairPrimary(false)}>{intl.formatMessage({ id: 'users.primary.repair' })}</Button>
      </div>}
      {repairPreview && <Dialog open onOpenChange={open => !open && !repairing && setRepairPreview(null)}>
        <DialogContent><DialogHeader><DialogTitle>{intl.formatMessage({ id: 'users.primary.repair' })}</DialogTitle></DialogHeader>
          <p className="text-sm">{intl.formatMessage({ id: 'users.primary.confirm' })}</p>
          <ul className="max-h-64 overflow-auto text-sm">{repairPreview.members.map(member => <li key={member.user_id}>{member.display_name} → {agentName(member.agent_id)}</li>)}</ul>
          {repairPreview.excluded_personal_count > 0 && <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'users.primary.personalSelf' })}</p>}
          <DialogFooter>
            <Button variant="outline" disabled={repairing} onClick={() => setRepairPreview(null)}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
            <Button disabled={repairing || repairPreview.members.length === 0} onClick={() => void repairPrimary(true)}>{intl.formatMessage({ id: 'common.confirm' })}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>}
      {isAdmin && (avatarConflicts === null ? <p role="alert">{intl.formatMessage({ id: 'users.avatar.loadError' })}</p> : avatarConflicts.length > 0 && (
        <div className="space-y-2 rounded-lg border border-surface-border p-4">
          <p className="font-medium">{intl.formatMessage({ id: 'users.avatar.conflicts' })}</p>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'users.avatar.conflictsHint' })}</p>
          <ul className="space-y-1 text-sm">{avatarConflicts.map(conflict => <li key={conflict.agent_id}>
            {agentName(conflict.agent_id)}: {conflict.user_ids.map(id => nameOf(id) || t('unknownMember')).join(', ')}
          </li>)}</ul>
        </div>
      ))}
      <OrgHistorySettings />
      <AdminMfa />

      {fetchError && (
        <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-4 py-3 text-sm text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <span>{fetchError}</span>
        </div>
      )}

      {signin.error && <p role="alert" className="text-sm text-destructive">{signin.error}</p>}

      {!loading && (users.length > 0 || signin.available) && (
        <Segmented<UsersView>
          value={currentView}
          onValueChange={setView}
          aria-label={intl.formatMessage({ id: 'users.view.label' })}
          options={[
            { value: 'list', label: intl.formatMessage({ id: 'users.view.list' }) },
            { value: 'org', label: intl.formatMessage({ id: 'users.view.org' }) },
            ...(signin.available ? [{ value: 'pending' as const, label: <span className="inline-flex items-center gap-1.5">{intl.formatMessage({ id: 'users.signinRequests.segment' })} <span className="rounded-full bg-muted px-1.5 tabular-nums">{signin.requests.length}</span></span> }] : []),
          ]}
        />
      )}

      {memberError && !memberConfirm && <p role="alert" className="text-sm text-destructive">{memberError}</p>}
      {isAdmin && currentView === 'list' && selected.length > 0 && <div className="flex flex-wrap items-center gap-3 rounded-lg bg-primary px-4 py-3 text-sm text-primary-foreground">
        <span role="status">{t('selected', { count: selected.length })}</span>
        <Button variant="secondary" disabled={memberBusy} onClick={() => { setMemberError(null); setBatchOpen(true); }}>{t('batchAction')}</Button>
        <Button variant="secondary" disabled={memberBusy} onClick={() => setSelected([])}>{t('clearSelection')}</Button>
      </div>}

      {loading ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="size-6 animate-spin text-muted-foreground" />
        </div>
      ) : currentView === 'pending' ? (
        <SigninRequestsPanel requests={signin.requests} onChanged={async () => { await Promise.all([signin.refresh(), fetchUsers()]); }} />
      ) : users.length === 0 ? (
        <Empty icon={Users} title={intl.formatMessage({ id: 'users.title' })} />
      ) : currentView === 'org' ? (
        <OrgChartView users={users} onEdit={setShowEdit} />
      ) : (
        isMobile ? (
          // Phones: one stacked card per person (the six-column grid does
          // not fit, and the page must not scroll sideways).
          <ul className="space-y-3">
            {users.map((user) => (
              <li key={user.id} className="space-y-2 rounded-xl border border-surface-border bg-card px-4 py-3">
                <div className="flex items-start gap-2">
                  {selectionFor(user)}
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
                  <InviteStatusChip state={inviteStates[user.id]} />
                </div>
                <div className="flex flex-col gap-0.5">
                  <RoleDetails user={user} managerName={nameOf(user.manager_user_id)} />
                  <UserAvatarStatus user={user} agentName={agentName} />
                  {isAdmin && <MemberSigninBadges user={user} credential={credentials[user.id]} />}
                  {avatarAction(user)}
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
                    <IdentityChips
                      state={identities[user.id]}
                      onAdd={() => setShowIdentityBind(user.id)}
                      {...(isAdmin ? { onRemove: (ident: ChannelIdentity) => void handleIdentityRemove(user.id, ident) } : {})}
                    />
                  </div>
                </div>
              </li>
            ))}
          </ul>
        ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={isAdmin ? `2.5rem ${COLUMNS}` : COLUMNS}
            className="!h-auto [&>[aria-hidden]]:hidden"
            header={
              <ListGridHeader>
                {isAdmin && <ListGridHeaderCell aria-label={t('selectionColumn')} />}
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
                {isAdmin && <ListGridCell>{selectionFor(user)}</ListGridCell>}
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
                  <UserAvatarStatus user={user} agentName={agentName} />
                  {isAdmin && <MemberSigninBadges user={user} credential={credentials[user.id]} />}
                </ListGridCell>
                <ListGridCell className="flex-wrap gap-1 py-1.5">
                  <BindingChips user={user} agentName={agentName} onUnbind={(a) => handleUnbind(user.id, a)} />
                  {avatarAction(user)}
                </ListGridCell>
                <ListGridCell className="flex-wrap gap-1 py-1.5">
                  <IdentityChips
                    state={identities[user.id]}
                    onAdd={() => setShowIdentityBind(user.id)}
                    {...(isAdmin ? { onRemove: (ident: ChannelIdentity) => void handleIdentityRemove(user.id, ident) } : {})}
                  />
                </ListGridCell>
                <ListGridCell className="flex-col items-start gap-1">
                  <StatusBadge status={user.status} />
                  <InviteStatusChip state={inviteStates[user.id]} />
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

      {stepUp.dialog}
      <RiskConfirmDialog open={batchOpen} onClose={() => { if (!memberBusy) setBatchOpen(false); }} onConfirm={attestSelected}
        title={t('batchTitle', { count: selected.length })} body={t('batchBody', { count: selected.length })}
        acknowledgeLabel={t('acknowledge')} auditNote={t('audit')} confirmLabel={t('batchConfirm', { count: selected.length })} busy={memberBusy} />
      {memberConfirm && <ConfirmDialog open onClose={() => { if (!memberBusy) setMemberConfirm(null); }}
        onConfirm={() => void memberAction(() => memberConfirm.action === 'unlink' ? memberSignin.unlink(memberConfirm.user.id) : memberSignin.revoke(memberConfirm.user.id))}
        title={t(`${memberConfirm.action}Title`, { name: memberConfirm.user.display_name })}
        message={t(`${memberConfirm.action}Body`)} confirmLabel={t(memberConfirm.action)} busy={memberBusy}>
        {memberError && <p role="alert" className="text-sm text-destructive">{memberError}</p>}
      </ConfirmDialog>}
      {inviteLink && <InviteLinkDialog link={inviteLink} onClose={() => setInviteLink(null)} />}
      {isAdmin && createAvatarFor && <CreateAvatarDialog
        user={createAvatarFor}
        onClose={() => setCreateAvatarFor(null)}
        onCreated={() => {
          setCreateAvatarFor(null);
          void fetchUsers();
          void useAgentsStore.getState().fetchAgents();
        }}
      />}
      {inviteUser && <Dialog open onOpenChange={open => !open && !inviting && setInviteUser(null)}>
        <DialogContent><DialogHeader><DialogTitle>{intl.formatMessage({ id: 'users.invite.resend' })}</DialogTitle></DialogHeader>
          <InviteEmailPreview email={inviteUser.email} />
          <DialogFooter><Button variant="outline" disabled={inviting} onClick={() => setInviteUser(null)}>{intl.formatMessage({ id: 'common.cancel' })}</Button><Button disabled={inviting} onClick={() => void sendInvite()}>{intl.formatMessage({ id: 'users.invite.send' })}</Button></DialogFooter>
        </DialogContent>
      </Dialog>}
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
