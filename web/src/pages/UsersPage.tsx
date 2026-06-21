import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type UserDetail } from '@/lib/api';
import { Dialog } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import { Users, UserPlus, Pencil, Link2, UserX, Trash2, X } from 'lucide-react';
import {
  Page,
  PageHeader,
  Card,
  Badge,
  Button,
  EmptyState,
  Field,
  controlClass,
} from '@/components/ui';

export function UsersPage() {
  const intl = useIntl();
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

  const statusTone = (status: string): 'success' | 'warning' | 'neutral' => {
    if (status === 'active') return 'success';
    if (status === 'suspended') return 'warning';
    return 'neutral';
  };

  const roleTone = (r: string): 'danger' | 'info' | 'neutral' => {
    if (r === 'admin') return 'danger';
    if (r === 'manager') return 'info';
    return 'neutral';
  };

  return (
    <Page>
      <PageHeader
        icon={Users}
        title={intl.formatMessage({ id: 'nav.users' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
        actions={
          <Button variant="primary" icon={UserPlus} onClick={() => setShowCreate(true)}>
            {intl.formatMessage({ id: 'users.create' })}
          </Button>
        }
      />

      {fetchError && (
        <div className="rounded-lg bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:bg-rose-900/20 dark:text-rose-400">
          {fetchError}
        </div>
      )}

      {loading ? (
        <Card>
          <div className="py-12 text-center text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.loading' })}
          </div>
        </Card>
      ) : users.length === 0 ? (
        <Card>
          <EmptyState
            icon={Users}
            title={intl.formatMessage({ id: 'users.title' })}
            action={
              <Button variant="primary" icon={UserPlus} onClick={() => setShowCreate(true)}>
                {intl.formatMessage({ id: 'users.create' })}
              </Button>
            }
          />
        </Card>
      ) : (
        <Card padded={false}>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead className="border-b border-[var(--panel-border)] bg-stone-500/5 dark:bg-white/5">
                <tr>
                  <th className="px-4 py-3 text-left font-medium text-stone-600 dark:text-stone-400">
                    {intl.formatMessage({ id: 'users.col.name' })}
                  </th>
                  <th className="px-4 py-3 text-left font-medium text-stone-600 dark:text-stone-400">
                    {intl.formatMessage({ id: 'users.col.email' })}
                  </th>
                  <th className="px-4 py-3 text-left font-medium text-stone-600 dark:text-stone-400">
                    {intl.formatMessage({ id: 'users.col.role' })}
                  </th>
                  <th className="px-4 py-3 text-left font-medium text-stone-600 dark:text-stone-400">
                    {intl.formatMessage({ id: 'users.col.status' })}
                  </th>
                  <th className="px-4 py-3 text-left font-medium text-stone-600 dark:text-stone-400">
                    {intl.formatMessage({ id: 'users.col.agents' })}
                  </th>
                  <th className="px-4 py-3 text-right font-medium text-stone-600 dark:text-stone-400">
                    {intl.formatMessage({ id: 'users.col.actions' })}
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-[var(--panel-border)]">
                {users.map((user) => (
                  <tr
                    key={user.id}
                    className="transition-colors hover:bg-stone-500/5 dark:hover:bg-white/5"
                  >
                    <td className="px-4 py-3 font-medium text-stone-900 dark:text-stone-100">
                      {user.display_name}
                    </td>
                    <td className="px-4 py-3 text-stone-600 dark:text-stone-400">{user.email}</td>
                    <td className="px-4 py-3">
                      <Badge tone={roleTone(user.role)}>{user.role}</Badge>
                    </td>
                    <td className="px-4 py-3">
                      <Badge tone={statusTone(user.status)}>{user.status}</Badge>
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex flex-wrap gap-1">
                        {user.bindings.map((b) => (
                          <span
                            key={b.agent_name}
                            className="inline-flex items-center gap-1 rounded-md bg-stone-500/10 px-2 py-0.5 text-xs text-stone-600 dark:text-stone-300"
                          >
                            {b.agent_name}
                            <span className="text-stone-400">({b.access_level})</span>
                            <button
                              onClick={() => handleUnbind(user.id, b.agent_name)}
                              className="rounded-full p-0.5 text-stone-400 hover:bg-rose-100 hover:text-rose-600 dark:hover:bg-rose-900/30"
                              title={intl.formatMessage({ id: 'users.action.unbind' })}
                              aria-label={`unbind ${b.agent_name}`}
                            >
                              <X className="h-3 w-3" />
                            </button>
                          </span>
                        ))}
                        {user.bindings.length === 0 && (
                          <span className="text-xs text-stone-400">—</span>
                        )}
                      </div>
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          variant="ghost"
                          size="sm"
                          icon={Pencil}
                          onClick={() => setShowEdit(user)}
                          title={intl.formatMessage({ id: 'users.action.edit' })}
                          aria-label={intl.formatMessage({ id: 'users.action.edit' })}
                        />
                        <Button
                          variant="ghost"
                          size="sm"
                          icon={Link2}
                          onClick={() => setShowBind(user.id)}
                          title={intl.formatMessage({ id: 'users.action.bind' })}
                          aria-label={intl.formatMessage({ id: 'users.action.bind' })}
                        />
                        {user.status === 'active' && (
                          <Button
                            variant="ghost"
                            size="sm"
                            icon={UserX}
                            onClick={() => setShowOffboard(user)}
                            title={intl.formatMessage({ id: 'users.action.offboard' })}
                            aria-label={intl.formatMessage({ id: 'users.action.offboard' })}
                            className="text-rose-500 hover:bg-rose-50 hover:text-rose-700 dark:hover:bg-rose-900/20 dark:hover:text-rose-400"
                          />
                        )}
                        <Button
                          variant="ghost"
                          size="sm"
                          icon={Trash2}
                          onClick={() => setShowRemove(user)}
                          title={intl.formatMessage({ id: 'users.action.remove' })}
                          aria-label={intl.formatMessage({ id: 'users.action.remove' })}
                          className="text-rose-500 hover:bg-rose-50 hover:text-rose-700 dark:hover:bg-rose-900/20 dark:hover:text-rose-400"
                        />
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Card>
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
    </Page>
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
  const [error, setError] = useState('');

  const handleConfirm = async () => {
    setError('');
    setSubmitting(true);
    try {
      await api.users.remove(user.id);
      toast.success(intl.formatMessage({ id: 'users.removed' }));
      onRemoved();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open title={intl.formatMessage({ id: 'users.action.remove' })} onClose={onClose}>
      <div className="space-y-4">
        <p className="text-sm text-stone-600 dark:text-stone-400">
          {intl.formatMessage({ id: 'users.remove.confirm' }, { name: user.display_name })}
        </p>
        {error && (
          <p className="rounded bg-rose-50 px-3 py-2 text-sm text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="danger" onClick={handleConfirm} disabled={submitting}>
            {submitting
              ? intl.formatMessage({ id: 'common.loading' })
              : intl.formatMessage({ id: 'common.delete' })}
          </Button>
        </div>
      </div>
    </Dialog>
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
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async () => {
    setError('');
    // Frontend validation (MEDIUM-1 fix)
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
      setError(intl.formatMessage({ id: 'users.error.invalidEmail' }));
      return;
    }
    if (password.length < 8) {
      setError(intl.formatMessage({ id: 'users.error.passwordTooShort' }));
      return;
    }
    setSubmitting(true);
    try {
      await api.users.create({ email, display_name: displayName, password, role: userRole });
      onCreated();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to create user');
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open title={intl.formatMessage({ id: 'users.create' })} onClose={onClose}>
      <div className="space-y-4">
        {error && (
          <p className="rounded bg-rose-50 px-3 py-2 text-sm text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
            {error}
          </p>
        )}
        <Field label={intl.formatMessage({ id: 'users.field.email' })}>
          <input
            type="email"
            placeholder={intl.formatMessage({ id: 'users.field.email' })}
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            className={controlClass}
          />
        </Field>
        <Field label={intl.formatMessage({ id: 'users.field.display_name' })}>
          <input
            placeholder={intl.formatMessage({ id: 'users.field.display_name' })}
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            className={controlClass}
          />
        </Field>
        <Field label={intl.formatMessage({ id: 'users.field.password' })}>
          <input
            type="password"
            placeholder={intl.formatMessage({ id: 'users.field.password' })}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            className={controlClass}
          />
        </Field>
        <Field label={intl.formatMessage({ id: 'users.col.role' })}>
          <select
            value={userRole}
            onChange={(e) => setUserRole(e.target.value)}
            className={controlClass}
          >
            <option value="employee">Employee</option>
            <option value="manager">Manager</option>
            <option value="admin">Admin</option>
          </select>
        </Field>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button
            variant="primary"
            onClick={handleSubmit}
            disabled={submitting || !email || !displayName || !password}
          >
            {intl.formatMessage({ id: 'common.create' })}
          </Button>
        </div>
      </div>
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
  const [newPassword, setNewPassword] = useState('');
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);

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
      await api.users.update({
        user_id: user.id,
        display_name: displayName !== user.display_name ? displayName : undefined,
        role: userRole !== user.role ? userRole : undefined,
        password: newPassword || undefined,
      });
      onUpdated();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open title={intl.formatMessage({ id: 'users.action.edit' })} onClose={onClose}>
      <div className="space-y-4">
        {error && (
          <p className="rounded bg-rose-50 px-3 py-2 text-sm text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
            {error}
          </p>
        )}
        <Field label={intl.formatMessage({ id: 'users.field.display_name' })}>
          <input
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            className={controlClass}
          />
        </Field>
        <Field label={intl.formatMessage({ id: 'users.col.role' })}>
          <select
            value={userRole}
            onChange={(e) => setUserRole(e.target.value)}
            className={controlClass}
          >
            <option value="employee">Employee</option>
            <option value="manager">Manager</option>
            <option value="admin">Admin</option>
          </select>
        </Field>
        <Field label={intl.formatMessage({ id: 'users.field.new_password' })}>
          <input
            type="password"
            placeholder={intl.formatMessage({ id: 'users.field.new_password' })}
            value={newPassword}
            onChange={(e) => setNewPassword(e.target.value)}
            className={controlClass}
          />
        </Field>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="primary" onClick={handleSubmit} disabled={submitting}>
            {intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

function BindAgentDialog({
  userId,
  onClose,
  onBound,
}: {
  userId: string;
  onClose: () => void;
  onBound: () => void;
}) {
  const intl = useIntl();
  const [agentName, setAgentName] = useState('');
  const [accessLevel, setAccessLevel] = useState('owner');
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);

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
    <Dialog open title={intl.formatMessage({ id: 'users.action.bind' })} onClose={onClose}>
      <div className="space-y-4">
        {error && (
          <p className="rounded bg-rose-50 px-3 py-2 text-sm text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
            {error}
          </p>
        )}
        <Field label={intl.formatMessage({ id: 'users.field.agent_name' })}>
          <input
            placeholder={intl.formatMessage({ id: 'users.field.agent_name' })}
            value={agentName}
            onChange={(e) => setAgentName(e.target.value)}
            className={controlClass}
          />
        </Field>
        <Field label={intl.formatMessage({ id: 'users.action.bind' })}>
          <select
            value={accessLevel}
            onChange={(e) => setAccessLevel(e.target.value)}
            className={controlClass}
          >
            <option value="owner">Owner</option>
            <option value="operator">Operator</option>
            <option value="viewer">Viewer</option>
          </select>
        </Field>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="primary" onClick={handleSubmit} disabled={submitting || !agentName}>
            {intl.formatMessage({ id: 'users.action.bind' })}
          </Button>
        </div>
      </div>
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
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async () => {
    setError('');
    setSubmitting(true);
    try {
      await api.users.offboard(user.id, transferTo || undefined);
      onOffboarded();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open title={intl.formatMessage({ id: 'users.action.offboard' })} onClose={onClose}>
      <div className="space-y-4">
        <p className="text-sm text-stone-600 dark:text-stone-400">
          {intl.formatMessage({ id: 'users.offboard.confirm' }, { name: user.display_name })}
        </p>
        {user.bindings.length > 0 && (
          <div>
            <p className="text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'users.offboard.agents' })}
            </p>
            <div className="mt-1 flex flex-wrap gap-1">
              {user.bindings.map((b) => (
                <span
                  key={b.agent_name}
                  className="rounded-md bg-stone-500/10 px-2 py-0.5 text-xs text-stone-600 dark:text-stone-300"
                >
                  {b.agent_name}
                </span>
              ))}
            </div>
            <select
              value={transferTo}
              onChange={(e) => setTransferTo(e.target.value)}
              className={`mt-2 ${controlClass}`}
            >
              <option value="">{intl.formatMessage({ id: 'users.offboard.no_transfer' })}</option>
              {users.map((u) => (
                <option key={u.id} value={u.id}>
                  {u.display_name} ({u.email})
                </option>
              ))}
            </select>
          </div>
        )}
        {error && (
          <p className="rounded bg-rose-50 px-3 py-2 text-sm text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="danger" onClick={handleSubmit} disabled={submitting}>
            {intl.formatMessage({ id: 'users.action.offboard' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
