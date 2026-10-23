// Shared pieces of the Users page dialogs (moved out of UsersPage.tsx
// unchanged, so the page file stays small).
import { useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { formatError } from '@/lib/toast';
import { orgErrorMessageId } from '@/lib/org-tree';
import { Input } from '@/components/mds';


/** Channels the backend's admin-prefill bind endpoint accepts (mirrors
 * `OTP_CHANNELS` in `server.rs::handle_channel_bind` — keep in sync). */
export const IDENTITY_CHANNELS: ReadonlyArray<{ value: string; label: string }> = [
  { value: 'telegram', label: 'Telegram' },
  { value: 'line', label: 'LINE' },
  { value: 'discord', label: 'Discord' },
  { value: 'slack', label: 'Slack' },
];

export function channelLabel(value: string): string {
  return IDENTITY_CHANNELS.find((c) => c.value === value)?.label ?? value;
}

/** Local option shape shared by the role / access / agent pickers below. */
export interface Option {
  value: string;
  label: string;
}

// Plain-language options paired with the raw enum value that actually gets
// written to the RPC payload.
export function roleOptions(intl: ReturnType<typeof useIntl>): Option[] {
  return [
    { value: 'employee', label: intl.formatMessage({ id: 'users.role.employee' }) },
    { value: 'manager', label: intl.formatMessage({ id: 'users.role.manager' }) },
    { value: 'admin', label: intl.formatMessage({ id: 'users.role.admin' }) },
  ];
}

export function accessOptions(intl: ReturnType<typeof useIntl>): Option[] {
  return [
    { value: 'owner', label: intl.formatMessage({ id: 'users.access.owner' }) },
    { value: 'operator', label: intl.formatMessage({ id: 'users.access.operator' }) },
    { value: 'viewer', label: intl.formatMessage({ id: 'users.access.viewer' }) },
  ];
}

/** Fetch existing department names once (for the create/edit datalist). */
export function useDepartmentNames(): string[] {
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
export function DepartmentInput({
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
export function DialogField({
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

/** The message for a refused create / update: a reporting-line rule in
 *  plain words when the gateway sent one of its codes, else the error text. */
export function userWriteError(e: unknown, intl: ReturnType<typeof useIntl>): string {
  const id = orgErrorMessageId(e);
  if (id) return intl.formatMessage({ id });
  return e instanceof Error ? e.message : formatError(e);
}
