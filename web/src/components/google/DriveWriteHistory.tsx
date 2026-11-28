import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { useUserNames } from '@/lib/use-user-names';
import { formatDateTime, useCompanyTimeZone } from '@/lib/company-time';
import { api, type UnifiedAuditEvent } from '@/lib/api';
import { DRIVE_WRITE_REASONS } from '@/lib/drive-write-reasons';

type Row = Record<string, unknown>;
function object(value: unknown): Row { return value && typeof value === 'object' && !Array.isArray(value) ? value as Row : {}; }
function text(value: unknown): string { return typeof value === 'string' ? value : ''; }
export function isDriveWriteEvent(event: UnifiedAuditEvent): boolean {
  return event.source === 'tool_call' && object(event.details?.tool_call).tool_name === 'google_drive_write';
}
const COLUMNS = ['time', 'employee', 'identity', 'requester', 'action', 'file', 'folder', 'approval', 'result', 'postCheck'];
const IDENTITIES: Record<string, string> = { personal: 'personal', home_oauth: 'home', home_sa: 'home', delegate: 'delegate', delegate_owner: 'delegateOwner' };
const ACTIONS = ['create', 'modify', 'move', 'trash'];
const MODES = ['none', 'member', 'member_and_admin'];
const OUTCOMES = ['not_sent', 'may_have_sent', 'confirmed', 'refused'];
const CHECKS = ['ok', 'violation', 'incomplete', 'none'];

/** A closed projection: never render the raw audit payload or approval ids. */
export function DriveWriteHistory({ events }: { events: UnifiedAuditEvent[] }) {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `driveWrite.${key}` });
  const agents = useAgentsStore(s => s.agents);
  const { nameOf } = useUserNames();
  const [folderNames, setFolderNames] = useState<Map<string, string>>(new Map());
  useEffect(() => {
    let active = true;
    // Audit records carry folder ids only. Resolve current display names when
    // available; removed/unreadable rules retain a plain-language label.
    void api.googleDriveWrite.get().then(status => {
      if (active) setFolderNames(new Map(status.folders.filter(folder => folder.kind !== 'unverified' && folder.name).map(folder => [folder.id, folder.name])));
    }).catch(() => { /* The audit remains readable without current settings. */ });
    return () => { active = false; };
  }, []);
  useCompanyTimeZone();
  const person = (value: unknown): string => {
    const raw = text(value);
    // RequesterIdentity is currently serialized using Rust Debug, not JSON.
    if (/^System \{ kind: "(?:cron|schedule|scheduled)"[, ]/.test(raw) || ['cron', 'schedule', 'scheduled'].includes(raw)) return t('person.schedule');
    if (/^System \{/.test(raw) || ['Unknown', 'unknown', 'system', 'heartbeat', 'dispatch', 'external', 'public_visitor'].includes(raw) || !raw) return t('person.system');
    if (/^(?:DashboardUser|ChannelUser) \{/.test(raw)) {
      const id = raw.match(/\buser_id: (?:Some\()?"([^"\\]+)"\)?(?:,| \})/)?.[1];
      return id ? nameOf(id) || t('person.system') : t('person.system');
    }
    const dashboardId = raw.match(/^dashboard:([^:]+)$/)?.[1];
    if (dashboardId) return nameOf(dashboardId) || t('person.system');
    // Channel principals are not dashboard user ids; never expose them.
    if (raw.includes(':') || /[{}]/.test(raw)) return t('person.system');
    if (raw.includes('@')) return raw;
    return nameOf(raw) || t('person.system');
  };
  return <div className="overflow-x-auto rounded-lg border border-surface-border">
    <table className="w-full text-left text-sm"><thead className="bg-muted text-xs text-muted-foreground"><tr>{COLUMNS.map(column => <th scope="col" key={column} className="whitespace-nowrap p-3 font-medium">{t(`column.${column}`)}</th>)}</tr></thead>
      <tbody>{events.map((event, index) => {
        const row = object(event.details?.tool_call);
        const approval = object(row.approval);
        const identity = IDENTITIES[text(row.identity_kind)];
        const action = text(row.action);
        const mode = text(approval.mode);
        const outcome = text(row.outcome);
        const check = text(row.post_check);
        const reason = text(row.reason);
        const agent = agents.find(item => item.name === event.agent_id || item.display_name === event.agent_id);
        const deciders = Array.isArray(approval.decided_by) ? [...new Set(approval.decided_by.map(person))].join('、') : '';
        const folder = text(row.folder_rule);
        const cells = [
          formatDateTime(event.timestamp, { locale: intl.locale }), agent?.display_name || t('unnamedEmployee'),
          <><span>{identity ? t(`identity.${identity}`) : t('person.system')}</span>{text(row.account_email).includes('@') && <span className="mt-1 block">{text(row.account_email)}</span>}</>,
          person(row.requester), ACTIONS.includes(action) ? t(`action.${action}`) : t('unfinished'), text(row.file_name) || '—',
          folder ? folder === 'global' ? t('rule.global') : folder.split(',').map(id => folderNames.get(id) || t('rule.limited')).join('、') : '—',
          <>{MODES.includes(mode) ? t(`approval.${mode}`) : '—'}{deciders && <span className="mt-1 block">{deciders}</span>}</>,
          <><span>{OUTCOMES.includes(outcome) ? t(`outcome.${outcome}`) : t('unfinished')}</span>{reason && <span className="mt-1 block text-muted-foreground">{DRIVE_WRITE_REASONS.includes(reason) ? t(`reason.${reason}`) : t('unfinished')}</span>}</>,
          CHECKS.includes(check) ? t(`postCheck.${check}`) : t('unfinished'),
        ];
        return <tr key={`${event.timestamp}-${index}`} className="border-t border-surface-border align-top">{cells.map((cell, cellIndex) => <td key={COLUMNS[cellIndex]} className="min-w-28 break-words p-3">{cell}</td>)}</tr>;
      })}</tbody>
    </table>
  </div>;
}
