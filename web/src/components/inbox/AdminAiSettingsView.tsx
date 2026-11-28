import { useIntl } from 'react-intl';
import { Badge, Button } from '@/components/mds';
import { describeCron } from '@/components/settings/controls/cron';
import { useNowTick } from '@/hooks/useNowTick';
import type { ApprovalItem } from '@/lib/api';
import { timeRemaining } from '@/lib/format';
import { expiryState } from '@/lib/inbox-model';
import { formatError, formatErrorDetail } from '@/lib/toast';

type ScheduleValue = Partial<Record<'name' | 'cron' | 'timezone' | 'task', string>> & { enabled?: boolean };
type SettingValue = string | number | boolean | null | ScheduleValue;
interface SettingsPayload {
  id: string;
  admin: string;
  avatar: string;
  employee_count: number;
  item_count: number;
  items: { seq: number; target: string; employee: string; label: string; before: SettingValue; after: SettingValue }[];
  reason: string | null;
  expires_at: string | null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === 'object' && !Array.isArray(value);
}

function isSettingValue(value: unknown): value is SettingValue {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return true;
  if (typeof value === 'number') return Number.isFinite(value);
  if (!isRecord(value) || Object.keys(value).length === 0) return false;
  return Object.entries(value).every(([key, field]) => key === 'enabled'
    ? typeof field === 'boolean'
    : ['name', 'cron', 'timezone', 'task'].includes(key) && typeof field === 'string');
}

export function parseAdminAiSettings(payload: unknown): SettingsPayload | null {
  if (!isRecord(payload)) return null;
  const count = (value: unknown) => typeof value === 'number' && Number.isSafeInteger(value) && value > 0;
  if (typeof payload.id !== 'string' || typeof payload.admin !== 'string' || !payload.admin.trim()
    || typeof payload.avatar !== 'string' || !count(payload.employee_count) || !count(payload.item_count)
    || !Array.isArray(payload.items) || payload.items.length !== payload.item_count
    || !(payload.reason === null || typeof payload.reason === 'string')
    || !(payload.expires_at === null || typeof payload.expires_at === 'string')) return null;
  if (!payload.items.every((item) => isRecord(item) && count(item.seq)
    && typeof item.target === 'string' && typeof item.employee === 'string' && !!item.employee.trim()
    && typeof item.label === 'string' && !!item.label.trim()
    && isSettingValue(item.before) && isSettingValue(item.after))) return null;
  return payload as unknown as SettingsPayload;
}

export function AdminAiSettingsFacts({ payload }: { payload: SettingsPayload }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const cronText = (raw: string) => describeCron(raw, {
    hourly: (mm) => t('controls.cron.desc.hourly', { mm }),
    daily: (time) => t('controls.cron.desc.daily', { time }),
    weekly: (day, time) => t('controls.cron.desc.weekly', { day, time }),
    interval: (n) => t('controls.cron.desc.interval', { n }),
    custom: () => raw,
    weekdays: Array.from({ length: 7 }, (_, i) => t(`controls.cron.weekday.${i}`)),
  });
  const scalar = (value: string | number | boolean | null) => value === null ? t('approval.adminAi.none')
    : typeof value === 'boolean' ? t(value ? 'approval.adminAi.on' : 'approval.adminAi.off') : String(value);
  const valueView = (value: SettingValue) => {
    if (value === null || typeof value !== 'object') return scalar(value);
    return (['name', 'cron', 'timezone', 'enabled', 'task'] as const).map((field) => value[field] !== undefined && (
      <div key={field}>
        <span className="text-muted-foreground">{t(`approval.adminAi.schedule.${field}`)}: </span>
        <span className="whitespace-pre-wrap break-words">{field === 'cron' ? cronText(value.cron!) : scalar(value[field]!)}</span>
      </div>
    ));
  };
  return <div className="min-w-0 space-y-4">
      <section className="space-y-1 text-sm">
        <h4 className="text-xs font-medium text-muted-foreground">{t('approval.adminAi.source')}</h4>
        <p className="break-words">{t('approval.adminAi.admin')}: {payload.admin}</p>
        {payload.avatar.trim() && <p className="break-words">{t('approval.adminAi.avatar')}: {payload.avatar}</p>}
      </section>
      <section className="min-w-0 space-y-2">
        <h4 className="text-sm font-medium">{t('approval.adminAi.settings', { n: payload.item_count })}</h4>
        <div className="max-w-full overflow-x-auto rounded-lg border border-surface-border" tabIndex={0} role="region" aria-label={t('approval.adminAi.settings', { n: payload.item_count })}>
          <table className="w-full min-w-[36rem] table-fixed text-left text-sm">
            <thead className="text-xs text-muted-foreground"><tr>
              {['employee', 'label', 'before', 'after'].map((key) => <th key={key} scope="col" className="border-b border-surface-border p-3 font-medium">{t(`approval.adminAi.${key}`)}</th>)}
            </tr></thead>
            <tbody>{payload.items.map((item, index) => <tr key={index}>
              <td className="border-b border-surface-border p-3 align-top break-words">{item.employee}</td>
              <td className="border-b border-surface-border p-3 align-top break-words">{item.label}</td>
              <td className="whitespace-pre-wrap break-words border-b border-surface-border p-3 align-top">{valueView(item.before)}</td>
              <td className="whitespace-pre-wrap break-words border-b border-surface-border p-3 align-top">{valueView(item.after)}</td>
            </tr>)}</tbody>
          </table>
        </div>
      </section>
      {payload.reason?.trim() && <section className="space-y-1 text-sm"><h4 className="text-xs font-medium text-muted-foreground">{t('approval.adminAi.reason')}</h4><p className="whitespace-pre-wrap break-words">{payload.reason}</p></section>}
    </div>;
}

export function AdminAiSettingsView({ approval, payload, onApprove, onReject, decideError }: {
  approval: ApprovalItem;
  payload: SettingsPayload;
  onApprove: (expectedDigest?: string) => void;
  onReject: () => void;
  decideError?: unknown;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const expiresAt = approval.expires_at ?? Math.round(Date.parse(approval.created_at) / 1000) + approval.ttl_seconds;
  const now = useNowTick(Number.isFinite(expiresAt));
  const expiry = expiryState({ timestamp: approval.created_at, expiresAt }, now);
  return (
    <div className="min-w-0 space-y-4" data-testid="admin-ai-settings">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <h3 className="font-medium">{t('approvals.kind.admin_ai_settings')}</h3>
        {expiry && <Badge variant="secondary">{t('approval.adminAi.remaining', { time: timeRemaining(expiry.remainingMs) })}</Badge>}
      </div>
      <AdminAiSettingsFacts payload={payload} />
      <p className="rounded-lg bg-muted p-3 text-sm">{t('approval.adminAi.recheck')}</p>
      {decideError != null && <div role="alert" className="whitespace-pre-wrap break-words text-sm text-destructive">{formatErrorDetail(decideError) || formatError(decideError)}</div>}
      <div className="flex flex-wrap gap-2 border-t border-surface-border pt-3">
        <Button variant="outline" onClick={onReject}>{t('common.cancel')}</Button>
        <Button disabled={expiry?.expired} onClick={() => onApprove(approval.digest ?? undefined)}>{t('approval.adminAi.confirm', { n: payload.item_count })}</Button>
      </div>
    </div>
  );
}
