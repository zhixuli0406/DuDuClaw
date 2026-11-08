import { AlertTriangle } from 'lucide-react';
import { useIntl, type IntlShape } from 'react-intl';
import { Link } from 'react-router';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { Button } from '@/components/mds';
import type { IsolationReason, IsolationReasonCode } from '@/lib/api';

const COMPONENTS = ['launch_service', 'sandbox', 'network', 'storage', 'parse_worker',
  'update_service', 'mcp_boundary', 'runtime_adapters'] as const;

// Only known provider families may become user-facing labels.
const PROVIDER_FAMILIES: Record<string, string> = {
  google: 'google', notion: 'notion', github: 'github', odoo: 'odoo',
  database: 'database', channel: 'channel', channels: 'channel',
  search: 'search', model: 'model', models: 'model',
  computer: 'computer', computer_login: 'computer',
};

// Admin next step per reason: an existing settings page and a short hint.
const NEXT_STEPS: Record<IsolationReasonCode, { hint: string; to: string }> = {
  binding_evidence_missing: { hint: 'isolationReadiness.hint.binding', to: '/app/system/migration' },
  connector_setup_required: { hint: 'isolationReadiness.hint.connector', to: '/manage/integrations' },
  personal_binding_conflict: { hint: 'isolationReadiness.hint.personal', to: '/manage/users' },
  revoked: { hint: 'isolationReadiness.hint.revoked', to: '/manage/integrations' },
  service_unavailable: { hint: 'isolationReadiness.hint.temporary', to: '/app/system/security' },
};

interface ReasonView { key: string; message: string; step?: { hint: string; to: string } }

function describeReason(intl: IntlShape, reason: IsolationReason): ReasonView {
  const step = Object.hasOwn(NEXT_STEPS, reason.code) ? NEXT_STEPS[reason.code] : undefined;
  switch (reason.code) {
    case 'binding_evidence_missing':
      return { key: reason.code, step, message: intl.formatMessage({ id: 'isolationReadiness.reason.binding' }) };
    case 'connector_setup_required': {
      const family = reason.provider && Object.hasOwn(PROVIDER_FAMILIES, reason.provider)
        ? PROVIDER_FAMILIES[reason.provider] : undefined;
      return family
        ? { key: `${reason.code}:${family}`, step, message: intl.formatMessage({ id: 'isolationReadiness.reason.connector' }, {
          provider: intl.formatMessage({ id: `isolationReadiness.provider.${family}` }),
        }) }
        : { key: reason.code, step, message: intl.formatMessage({ id: 'isolationReadiness.reason.setup' }) };
    }
    case 'personal_binding_conflict':
      return { key: reason.code, step, message: intl.formatMessage({ id: 'isolationReadiness.personal' }) };
    case 'revoked':
      return { key: reason.code, step, message: intl.formatMessage({ id: 'isolationReadiness.reason.revoked' }) };
    case 'service_unavailable':
      return { key: reason.code, step, message: intl.formatMessage({ id: 'isolationReadiness.reason.temporary' }) };
    default:
      return { key: 'unknown', message: intl.formatMessage({ id: 'isolationReadiness.reason.setup' }) };
  }
}

function unique(views: ReasonView[]): ReasonView[] {
  const byKey = new Map<string, ReasonView>();
  for (const view of views) if (!byKey.has(view.key)) byKey.set(view.key, view);
  return [...byKey.values()];
}

/** Readiness diagnosis only; this component does not authorize execution. */
export function IsolationReadinessBanner() {
  const intl = useIntl();
  const readiness = useSystemStore(s => s.status?.isolation_readiness);
  const loading = useSystemStore(s => s.loading);
  const refresh = useSystemStore(s => s.fetchStatus);
  const personal = useSystemStore(s => s.status?.edition_profile === 'personal');
  const admin = useAuthStore(s => s.user?.role === 'admin');
  if (!readiness?.required) return null;
  const reasons = unique((readiness.reasons ?? []).map(reason => describeReason(intl, reason)));
  // Admission is open but some employees still need an administrator.
  const attention = readiness.ready && reasons.length > 0;
  if (readiness.ready && !attention) return null;
  const employees = admin ? (readiness.employees ?? []).filter(employee => !employee.ready) : [];

  return (
    <div role="alert" className="rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
        <div className="min-w-0 flex-1 space-y-1">
          <p className="font-medium">{intl.formatMessage({ id: attention
            ? 'isolationReadiness.titleAttention' : 'isolationReadiness.title' })}</p>
          <p className="text-muted-foreground">{intl.formatMessage({ id: attention
            ? 'isolationReadiness.attention'
            : readiness.reasons === undefined
              ? 'isolationReadiness.description'
              : personal ? 'isolationReadiness.personal' : 'isolationReadiness.notice' })}</p>
          {reasons.length > 0 && (
            <ul className="list-disc space-y-1 pl-5">
              {reasons.map(reason => (
                <li key={reason.key}>
                  {reason.message}
                  {admin && reason.step && (
                    <>
                      {' '}
                      <Link to={reason.step.to} className="underline">
                        {intl.formatMessage({ id: reason.step.hint })}
                      </Link>
                    </>
                  )}
                </li>
              ))}
            </ul>
          )}
          {employees.length > 0 && (
            <div className="space-y-1 pt-1">
              <p className="font-medium">{intl.formatMessage({ id: 'isolationReadiness.employees' })}</p>
              <ul className="list-disc space-y-1 pl-5">
                {employees.map(employee => (
                  <li key={employee.label}>
                    {intl.formatMessage({ id: 'isolationReadiness.employeeReasons' }, {
                      label: employee.label,
                      reasons: unique(employee.reasons.map(reason => describeReason(intl, reason)))
                        .map(reason => reason.message).join(' '),
                    })}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {admin && (
            <div className="space-y-1">
              <Link to="/app/system/security" className="underline">
                {intl.formatMessage({ id: 'isolationReadiness.settings' })}
              </Link>
              {readiness.correlation_id && (
                <p>{intl.formatMessage({ id: 'isolationReadiness.reference' }, { id: readiness.correlation_id })}</p>
              )}
            </div>
          )}
          {admin && (
            <details className="pt-1">
              <summary className="cursor-pointer">{intl.formatMessage({ id: 'isolationReadiness.details' })}</summary>
              <ul className="mt-2 space-y-1">
                {COMPONENTS.filter(name => readiness.components[name] !== 'ready').map(name => (
                  <li key={name}>
                    {intl.formatMessage({ id: `isolationReadiness.component.${name}` })}
                    {' — '}
                    {intl.formatMessage({ id: readiness.components[name] === 'pending_integration'
                      ? 'isolationReadiness.pending' : 'isolationReadiness.unavailable' })}
                  </li>
                ))}
              </ul>
            </details>
          )}
        </div>
        <Button variant="outline" size="sm" disabled={loading} onClick={() => { void refresh(); }}>
          {intl.formatMessage({ id: 'isolationReadiness.refresh' })}
        </Button>
      </div>
    </div>
  );
}
