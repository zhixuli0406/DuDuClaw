// Create / edit user dialogs of the Users page (moved out of UsersPage.tsx
// unchanged).
import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type UserDetail } from '@/lib/api';
import {
  Button,
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
} from '@/components/mds';
import { ManagerSelect } from './ManagerSelect';
import { DialogField, DepartmentInput, roleOptions, useDepartmentNames, userWriteError } from './dialog-common';

export function CreateUserDialog({
  users,
  onClose,
  onCreated,
}: {
  users: UserDetail[];
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const [email, setEmail] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [password, setPassword] = useState('');
  const [userRole, setUserRole] = useState('employee');
  const [department, setDepartment] = useState('');
  const [managerId, setManagerId] = useState('');
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
        ...(managerId ? { manager_user_id: managerId } : {}),
      });
      onCreated();
    } catch (e) {
      setError(userWriteError(e, intl));
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
          <DialogField
            label={intl.formatMessage({ id: 'users.manager.label' })}
            help={intl.formatMessage({ id: 'users.manager.hint' })}
          >
            <ManagerSelect users={users} userId={null} value={managerId} onChange={setManagerId} />
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

export function EditUserDialog({
  user,
  users,
  onClose,
  onUpdated,
}: {
  user: UserDetail;
  users: UserDetail[];
  onClose: () => void;
  onUpdated: () => void;
}) {
  const intl = useIntl();
  const [displayName, setDisplayName] = useState(user.display_name);
  const [userRole, setUserRole] = useState<string>(user.role);
  const [department, setDepartment] = useState(user.department ?? '');
  const [managerId, setManagerId] = useState(user.manager_user_id ?? '');
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
      // Same for the direct manager; `null` clears it.
      const managerChanged = managerId !== (user.manager_user_id ?? '');
      await api.users.update({
        user_id: user.id,
        display_name: displayName !== user.display_name ? displayName : undefined,
        role: userRole !== user.role ? userRole : undefined,
        password: newPassword || undefined,
        ...(deptChanged ? { department: department.trim() } : {}),
        ...(managerChanged ? { manager_user_id: managerId || null } : {}),
      });
      onUpdated();
    } catch (e) {
      setError(userWriteError(e, intl));
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
          <DialogField
            label={intl.formatMessage({ id: 'users.manager.label' })}
            help={intl.formatMessage({ id: 'users.manager.hint' })}
          >
            <ManagerSelect users={users} userId={user.id} value={managerId} onChange={setManagerId} />
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
