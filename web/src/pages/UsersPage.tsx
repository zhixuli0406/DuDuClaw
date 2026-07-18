import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { api, type UserDetail } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import type { UserRole } from '@/stores/auth-store';
import { ROLE_LEVELS } from '@/lib/roles';
import { ConfirmDialog } from '@/components/settings/controls';
import {
  Button,
  Badge,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
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
} from 'lucide-react';

/** Local option shape shared by the role / access / agent pickers below. */
interface Option {
  value: string;
  label: string;
}

// Plain-language options paired with the raw enum value that actually gets
// written to the RPC payload.
function roleOptions(intl: ReturnType<typeof useIntl>): Option[] {
  return [
    { value: 'employee', label: intl.formatMessage({ id: 'users.role.employee' }) },
    { value: 'manager', label: intl.formatMessage({ id: 'users.role.manager' }) },
    { value: 'admin', label: intl.formatMessage({ id: 'users.role.admin' }) },
  ];
}

function accessOptions(intl: ReturnType<typeof useIntl>): Option[] {
  return [
    { value: 'owner', label: intl.formatMessage({ id: 'users.access.owner' }) },
    { value: 'operator', label: intl.formatMessage({ id: 'users.access.operator' }) },
    { value: 'viewer', label: intl.formatMessage({ id: 'users.access.viewer' }) },
  ];
}

/** Fetch existing department names once (for the create/edit datalist). */
function useDepartmentNames(): string[] {
  const [names, setNames] = useState<string[]>([]);
  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const res = await api.departments.list();
        if (alive) setNames((res?.departments ?? []).map((d) => d.name));
      } catch {
        /* departments are optional; a free-text department still works */
      }
    })();
    return () => { alive = false; };
  }, []);
  return names;
}

/** A department picker: type-ahead over existing departments, free entry OK.
 *  Empty value = no department (clears on edit). */
function DepartmentInput({
  value,
  onChange,
  options,
}: {
  value: string;
  onChange: (v: string) => void;
  options: string[];
}) {
  const intl = useIntl();
  return (
    <>
      <Input
        list="dept-options"
        placeholder={intl.formatMessage({ id: 'users.field.departmentPlaceholder' })}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
      <datalist id="dept-options">
        {options.map((d) => (
          <option key={d} value={d} />
        ))}
      </datalist>
    </>
  );
}

/** Stacked label + control block used across the user dialogs (spec §5.3). */
function DialogField({
  label,
  help,
  children,
}: {
  label: string;
  help?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground">{label}</label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
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

const COLUMNS = 'minmax(0,1.6fr) minmax(0,0.9fr) minmax(0,1.8fr) minmax(0,0.7fr) 2.5rem';

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

      {loading ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="size-6 animate-spin text-muted-foreground" />
        </div>
      ) : users.length === 0 ? (
        <Empty icon={Users} title={intl.formatMessage({ id: 'users.title' })} />
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
            setShowBind(null);
            fetchUsers();
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

// ── UI.1 — delete-user confirm dialog ──

function RemoveUserDialog({
  user,
  onClose,
  onRemoved,
}: {
  user: UserDetail;
  onClose: () => void;
  onRemoved: () => void;
}) {
  const intl = useIntl();
  const [submitting, setSubmitting] = useState(false);

  const handleConfirm = async () => {
    setSubmitting(true);
    try {
      await api.users.remove(user.id);
      toast.success(intl.formatMessage({ id: 'users.removed' }));
      onRemoved();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      setSubmitting(false);
    }
  };

  return (
    <ConfirmDialog
      open
      onClose={onClose}
      onConfirm={handleConfirm}
      title={intl.formatMessage({ id: 'users.action.remove' })}
      message={intl.formatMessage({ id: 'users.remove.confirm' }, { name: user.display_name })}
      confirmLabel={intl.formatMessage({ id: 'common.delete' })}
      requireText={user.display_name}
      requireTextHint={intl.formatMessage({ id: 'users.remove.requireHint' })}
      busy={submitting}
    />
  );
}

// ── Sub-dialogs ──────────────────────────────────────────────

function CreateUserDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const [email, setEmail] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [password, setPassword] = useState('');
  const [userRole, setUserRole] = useState('employee');
  const [department, setDepartment] = useState('');
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const deptOptions = useDepartmentNames();
  const roles = roleOptions(intl);

  const handleSubmit = async () => {
    setError('');
    // Frontend validation (MEDIUM-1 fix). Do NOT require a dotted domain: the
    // seed admin account is `admin@local` and internal hostnames like
    // `manager.test@local` are legitimate (Bug#6). Match the backend's lenient
    // rule — a non-empty local part, an `@`, and a non-empty domain.
    if (!/^[^\s@]+@[^\s@]+$/.test(email)) {
      setError(intl.formatMessage({ id: 'users.error.invalidEmail' }));
      return;
    }
    if (password.length < 8) {
      setError(intl.formatMessage({ id: 'users.error.passwordTooShort' }));
      return;
    }
    setSubmitting(true);
    try {
      await api.users.create({
        email, display_name: displayName, password, role: userRole,
        ...(department.trim() ? { department: department.trim() } : {}),
      });
      onCreated();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to create user');
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'users.create' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          {error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>
          )}
          <DialogField label={intl.formatMessage({ id: 'users.field.email' })}>
            <Input
              type="email"
              placeholder={intl.formatMessage({ id: 'users.field.email' })}
              value={email}
              onChange={(e) => setEmail(e.target.value)}
            />
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'users.field.display_name' })}>
            <Input
              placeholder={intl.formatMessage({ id: 'users.field.display_name' })}
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
            />
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'users.field.password' })}>
            <Input
              type="password"
              placeholder={intl.formatMessage({ id: 'users.field.password' })}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'users.col.role' })}>
            <Select value={userRole} onValueChange={(v) => setUserRole(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{roles.find((r) => r.value === userRole)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {roles.map((r) => (
                  <SelectItem key={r.value} value={r.value}>{r.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
          <DialogField
            label={intl.formatMessage({ id: 'users.field.department' })}
            help={intl.formatMessage({ id: 'users.field.departmentHint' })}
          >
            <DepartmentInput value={department} onChange={setDepartment} options={deptOptions} />
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button
            variant="brand"
            onClick={handleSubmit}
            disabled={submitting || !email || !displayName || !password}
          >
            {intl.formatMessage({ id: 'common.create' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function EditUserDialog({
  user,
  onClose,
  onUpdated,
}: {
  user: UserDetail;
  onClose: () => void;
  onUpdated: () => void;
}) {
  const intl = useIntl();
  const [displayName, setDisplayName] = useState(user.display_name);
  const [userRole, setUserRole] = useState<string>(user.role);
  const [department, setDepartment] = useState(user.department ?? '');
  const [newPassword, setNewPassword] = useState('');
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const deptOptions = useDepartmentNames();
  const roles = roleOptions(intl);

  const handleSubmit = async () => {
    setError('');
    // Match CreateUserDialog's client-side rule so the user gets a clear
    // message instead of an opaque backend rejection.
    if (newPassword && newPassword.length < 8) {
      setError(intl.formatMessage({ id: 'users.error.passwordTooShort' }));
      return;
    }
    setSubmitting(true);
    try {
      // Send department only when it changed. An empty string clears it.
      const deptChanged = department.trim() !== (user.department ?? '').trim();
      await api.users.update({
        user_id: user.id,
        display_name: displayName !== user.display_name ? displayName : undefined,
        role: userRole !== user.role ? userRole : undefined,
        password: newPassword || undefined,
        ...(deptChanged ? { department: department.trim() } : {}),
      });
      onUpdated();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'users.action.edit' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          {error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>
          )}
          <DialogField label={intl.formatMessage({ id: 'users.field.display_name' })}>
            <Input value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'users.col.role' })}>
            <Select value={userRole} onValueChange={(v) => setUserRole(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{roles.find((r) => r.value === userRole)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {roles.map((r) => (
                  <SelectItem key={r.value} value={r.value}>{r.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
          <DialogField
            label={intl.formatMessage({ id: 'users.field.department' })}
            help={intl.formatMessage({ id: 'users.field.departmentHint' })}
          >
            <DepartmentInput value={department} onChange={setDepartment} options={deptOptions} />
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'users.field.new_password' })}>
            <Input
              type="password"
              placeholder={intl.formatMessage({ id: 'users.field.new_password' })}
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
            />
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="brand" onClick={handleSubmit} disabled={submitting}>
            {intl.formatMessage({ id: 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function BindAgentDialog({
  userId,
  boundAgents,
  onClose,
  onBound,
}: {
  userId: string;
  /** Agent names this user is already bound to — excluded from the picker. */
  boundAgents: string[];
  onClose: () => void;
  onBound: () => void;
}) {
  const intl = useIntl();
  const [agentName, setAgentName] = useState('');
  const [accessLevel, setAccessLevel] = useState('owner');
  const [agentOptions, setAgentOptions] = useState<Option[] | null>(null);
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const accesses = accessOptions(intl);

  useEffect(() => {
    let cancelled = false;
    api.agents
      .list()
      .then(({ agents }) => {
        if (cancelled) return;
        const options = agents
          .filter((a) => !boundAgents.includes(a.name))
          .map((a) => ({
            value: a.name,
            label: a.display_name && a.display_name !== a.name ? `${a.display_name} · ${a.name}` : a.name,
          }));
        setAgentOptions(options);
        setAgentName((prev) => prev || options[0]?.value || '');
      })
      .catch(() => {
        // Roster fetch failed — fall back to the free-text input below.
        if (!cancelled) setAgentOptions(null);
      });
    return () => {
      cancelled = true;
    };
    // boundAgents is derived fresh from the parent each open; the dialog
    // remounts per open so a one-shot fetch is enough.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleSubmit = async () => {
    setError('');
    setSubmitting(true);
    try {
      await api.users.bindAgent(userId, agentName, accessLevel);
      onBound();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'users.action.bind' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          {error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>
          )}
          <DialogField label={intl.formatMessage({ id: 'users.field.agent_name' })}>
            {agentOptions && agentOptions.length > 0 ? (
              <Select value={agentName} onValueChange={(v) => setAgentName(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>{agentOptions.find((o) => o.value === agentName)?.label ?? agentName}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {agentOptions.map((o) => (
                    <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : agentOptions && agentOptions.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'users.bind.none_available' })}
              </p>
            ) : (
              <Input
                placeholder={intl.formatMessage({ id: 'users.field.agent_name' })}
                value={agentName}
                onChange={(e) => setAgentName(e.target.value)}
              />
            )}
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'users.action.bind' })}>
            <Select value={accessLevel} onValueChange={(v) => setAccessLevel(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{accesses.find((a) => a.value === accessLevel)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {accesses.map((a) => (
                  <SelectItem key={a.value} value={a.value}>{a.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="brand" onClick={handleSubmit} disabled={submitting || !agentName}>
            {intl.formatMessage({ id: 'users.action.bind' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function OffboardDialog({
  user,
  users,
  onClose,
  onOffboarded,
}: {
  user: UserDetail;
  users: UserDetail[];
  onClose: () => void;
  onOffboarded: () => void;
}) {
  const intl = useIntl();
  const [transferTo, setTransferTo] = useState('');
  const [confirming, setConfirming] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const handleConfirm = async () => {
    setSubmitting(true);
    try {
      await api.users.offboard(user.id, transferTo || undefined);
      onOffboarded();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      setSubmitting(false);
    }
  };

  const hasBindings = user.bindings.length > 0;

  // No AI staff to reassign (or transferee already chosen) → route the
  // destructive step through the shared ConfirmDialog. Payload is unchanged:
  // offboard(user.id, transferTo || undefined).
  if (!hasBindings || confirming) {
    return (
      <ConfirmDialog
        open
        onClose={hasBindings ? () => setConfirming(false) : onClose}
        onConfirm={handleConfirm}
        title={intl.formatMessage({ id: 'users.action.offboard' })}
        message={intl.formatMessage({ id: 'users.offboard.confirm' }, { name: user.display_name })}
        confirmLabel={intl.formatMessage({ id: 'users.action.offboard' })}
        busy={submitting}
      />
    );
  }

  // Bindings present → pick a transferee first (preserves the transferTo field),
  // then hand off to ConfirmDialog above.
  const transferOptions: Option[] = [
    { value: '', label: intl.formatMessage({ id: 'users.offboard.no_transfer' }) },
    ...users.map((u) => ({ value: u.id, label: `${u.display_name} (${u.email})` })),
  ];

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'users.action.offboard' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'users.offboard.confirm' }, { name: user.display_name })}
          </p>
          <div>
            <p className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'users.offboard.agents' })}
            </p>
            <div className="mt-1 flex flex-wrap gap-1">
              {user.bindings.map((b) => (
                <span
                  key={b.agent_name}
                  className="inline-flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
                >
                  <ActorAvatar actorType="agent" name={b.agent_name} size="xs" />
                  {b.agent_name}
                </span>
              ))}
            </div>
          </div>
          <DialogField label={intl.formatMessage({ id: 'users.offboard.transfer' })}>
            <Select value={transferTo} onValueChange={(v) => setTransferTo(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{transferOptions.find((o) => o.value === transferTo)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {transferOptions.map((o) => (
                  <SelectItem key={o.value || 'none'} value={o.value}>{o.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="destructive" onClick={() => setConfirming(true)}>
            {intl.formatMessage({ id: 'users.action.offboard' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
