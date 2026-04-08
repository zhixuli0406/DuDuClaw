import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type UserDetail } from '@/lib/api';
import { Dialog } from '@/components/shared/Dialog';
import { Shield, UserPlus, Pencil, Link2, UserX } from 'lucide-react';

export function UsersPage() {
  const intl = useIntl();
  const [users, setUsers] = useState<UserDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [showBind, setShowBind] = useState<string | null>(null);
  const [showEdit, setShowEdit] = useState<UserDetail | null>(null);
  const [showOffboard, setShowOffboard] = useState<UserDetail | null>(null);

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

  const statusBadge = (status: string) => {
    const styles: Record<string, string> = {
      active: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
      suspended: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
      offboarded: 'bg-stone-100 text-stone-500 dark:bg-stone-800 dark:text-stone-400',
    };
    return (
      <span className={`inline-flex rounded-full px-2 py-0.5 text-xs font-medium ${styles[status] ?? styles.active}`}>
        {status}
      </span>
    );
  };

  const roleBadge = (r: string) => {
    const styles: Record<string, string> = {
      admin: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
      manager: 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400',
      employee: 'bg-stone-100 text-stone-600 dark:bg-stone-800 dark:text-stone-300',
    };
    return (
      <span className={`inline-flex rounded-full px-2 py-0.5 text-xs font-medium ${styles[r] ?? styles.employee}`}>
        {r}
      </span>
    );
  };

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Shield className="h-6 w-6 text-amber-500" />
          <h1 className="text-xl font-semibold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'users.title' })}
          </h1>
        </div>
        <button
          onClick={() => setShowCreate(true)}
          className="flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white shadow-sm transition-colors hover:bg-amber-600"
        >
          <UserPlus className="h-4 w-4" />
          {intl.formatMessage({ id: 'users.create' })}
        </button>
      </div>

      {/* Users Table */}
      {fetchError && (
        <div className="rounded-lg bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:bg-rose-900/20 dark:text-rose-400">
          {fetchError}
        </div>
      )}

      {loading ? (
        <div className="py-12 text-center text-stone-500">Loading...</div>
      ) : (
        <div className="overflow-hidden rounded-xl border border-stone-200 dark:border-stone-800">
          <table className="w-full text-sm">
            <thead className="border-b border-stone-200 bg-stone-50 dark:border-stone-800 dark:bg-stone-900">
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
            <tbody className="divide-y divide-stone-100 dark:divide-stone-800">
              {users.map((user) => (
                <tr
                  key={user.id}
                  className="bg-white transition-colors hover:bg-stone-50 dark:bg-stone-900 dark:hover:bg-stone-800/50"
                >
                  <td className="px-4 py-3 font-medium text-stone-900 dark:text-stone-100">
                    {user.display_name}
                  </td>
                  <td className="px-4 py-3 text-stone-600 dark:text-stone-400">{user.email}</td>
                  <td className="px-4 py-3">{roleBadge(user.role)}</td>
                  <td className="px-4 py-3">{statusBadge(user.status)}</td>
                  <td className="px-4 py-3">
                    <div className="flex flex-wrap gap-1">
                      {user.bindings.map((b) => (
                        <span
                          key={b.agent_name}
                          className="inline-flex items-center gap-1 rounded bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-300"
                        >
                          {b.agent_name}
                          <span className="text-stone-400">({b.access_level})</span>
                        </span>
                      ))}
                      {user.bindings.length === 0 && (
                        <span className="text-xs text-stone-400">—</span>
                      )}
                    </div>
                  </td>
                  <td className="px-4 py-3">
                    <div className="flex items-center justify-end gap-1">
                      <button
                        onClick={() => setShowEdit(user)}
                        className="rounded p-1.5 text-stone-500 transition-colors hover:bg-stone-100 hover:text-stone-700 dark:hover:bg-stone-800 dark:hover:text-stone-300"
                        title={intl.formatMessage({ id: 'users.action.edit' })}
                      >
                        <Pencil className="h-4 w-4" />
                      </button>
                      <button
                        onClick={() => setShowBind(user.id)}
                        className="rounded p-1.5 text-stone-500 transition-colors hover:bg-stone-100 hover:text-stone-700 dark:hover:bg-stone-800 dark:hover:text-stone-300"
                        title={intl.formatMessage({ id: 'users.action.bind' })}
                      >
                        <Link2 className="h-4 w-4" />
                      </button>
                      {user.status === 'active' && (
                        <button
                          onClick={() => setShowOffboard(user)}
                          className="rounded p-1.5 text-rose-500 transition-colors hover:bg-rose-50 hover:text-rose-700 dark:hover:bg-rose-900/20 dark:hover:text-rose-400"
                          title={intl.formatMessage({ id: 'users.action.offboard' })}
                        >
                          <UserX className="h-4 w-4" />
                        </button>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
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
    </div>
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
      setError('Invalid email format');
      return;
    }
    if (password.length < 8) {
      setError('Password must be at least 8 characters');
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
        <input
          type="email"
          placeholder={intl.formatMessage({ id: 'users.field.email' })}
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        />
        <input
          placeholder={intl.formatMessage({ id: 'users.field.display_name' })}
          value={displayName}
          onChange={(e) => setDisplayName(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        />
        <input
          type="password"
          placeholder={intl.formatMessage({ id: 'users.field.password' })}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        />
        <select
          value={userRole}
          onChange={(e) => setUserRole(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        >
          <option value="employee">Employee</option>
          <option value="manager">Manager</option>
          <option value="admin">Admin</option>
        </select>
        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-lg px-4 py-2 text-sm text-stone-600 transition-colors hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !email || !displayName || !password}
            className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            {intl.formatMessage({ id: 'common.create' })}
          </button>
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
        <input
          value={displayName}
          onChange={(e) => setDisplayName(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        />
        <select
          value={userRole}
          onChange={(e) => setUserRole(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        >
          <option value="employee">Employee</option>
          <option value="manager">Manager</option>
          <option value="admin">Admin</option>
        </select>
        <input
          type="password"
          placeholder={intl.formatMessage({ id: 'users.field.new_password' })}
          value={newPassword}
          onChange={(e) => setNewPassword(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        />
        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-lg px-4 py-2 text-sm text-stone-600 transition-colors hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting}
            className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            {intl.formatMessage({ id: 'common.save' })}
          </button>
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
        <input
          placeholder={intl.formatMessage({ id: 'users.field.agent_name' })}
          value={agentName}
          onChange={(e) => setAgentName(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        />
        <select
          value={accessLevel}
          onChange={(e) => setAccessLevel(e.target.value)}
          className="w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
        >
          <option value="owner">Owner</option>
          <option value="operator">Operator</option>
          <option value="viewer">Viewer</option>
        </select>
        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-lg px-4 py-2 text-sm text-stone-600 transition-colors hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !agentName}
            className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            {intl.formatMessage({ id: 'users.action.bind' })}
          </button>
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
                  className="rounded bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-300"
                >
                  {b.agent_name}
                </span>
              ))}
            </div>
            <select
              value={transferTo}
              onChange={(e) => setTransferTo(e.target.value)}
              className="mt-2 w-full rounded-lg border border-stone-300 px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
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
          <button
            onClick={onClose}
            className="rounded-lg px-4 py-2 text-sm text-stone-600 transition-colors hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting}
            className="rounded-lg bg-rose-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-rose-600 disabled:opacity-50"
          >
            {intl.formatMessage({ id: 'users.action.offboard' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
