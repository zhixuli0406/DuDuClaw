import { AlertTriangle } from 'lucide-react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { Button } from '@/components/mds';

const COMPONENTS = ['launch_service', 'sandbox', 'network', 'storage', 'parse_worker',
  'update_service', 'mcp_boundary', 'runtime_adapters'] as const;

// Only known provider families may become user-facing labels.
const PROVIDER_FAMILIES: Record<string, string> = {
  google: 'google', notion: 'notion', github: 'github', odoo: 'odoo',
  database: 'database', channel: 'channel', channels: 'channel',
  search: 'search', model: 'model', models: 'model',
  computer: 'computer', computer_login: 'computer',
};

/** Readiness diagnosis only; this component does not authorize execution. */
export function IsolationReadinessBanner() {
  const intl = useIntl();
  const readiness = useSystemStore(s => s.status?.isolation_readiness);
  const loading = useSystemStore(s => s.loading);
  const refresh = useSystemStore(s => s.fetchStatus);
  const personal = useSystemStore(s => s.status?.edition_profile === 'personal');
  const admin = useAuthStore(s => s.user?.role === 'admin');
  if (!readiness?.required || readiness.ready) return null;

  const reasonMessages = [...new Set(readiness.reasons?.map(reason => {
    switch (reason.code) {
      case 'binding_evidence_missing':
        return intl.formatMessage({ id: 'isolationReadiness.reason.binding' });
      case 'connector_setup_required': {
        const family = reason.provider && Object.hasOwn(PROVIDER_FAMILIES, reason.provider)
          ? PROVIDER_FAMILIES[reason.provider] : undefined;
        return family
          ? intl.formatMessage({ id: 'isolationReadiness.reason.connector' }, {
            provider: intl.formatMessage({ id: `isolationReadiness.provider.${family}` }),
          })
          : intl.formatMessage({ id: 'isolationReadiness.reason.setup' });
      }
      case 'personal_binding_conflict':
        return intl.formatMessage({ id: 'isolationReadiness.personal' });
      case 'revoked':
        return intl.formatMessage({ id: 'isolationReadiness.reason.revoked' });
      case 'service_unavailable':
        return intl.formatMessage({ id: 'isolationReadiness.reason.temporary' });
      default:
        return intl.formatMessage({ id: 'isolationReadiness.reason.setup' });
    }
  }))];

  return (
    <div role="alert" className="rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
        <div className="min-w-0 flex-1 space-y-1">
          <p className="font-medium">{intl.formatMessage({ id: 'isolationReadiness.title' })}</p>
          <p className="text-muted-foreground">{intl.formatMessage({ id: readiness.reasons === undefined
            ? 'isolationReadiness.description'
            : personal ? 'isolationReadiness.personal' : 'isolationReadiness.notice' })}</p>
          {reasonMessages.length > 0 && (
            <ul className="list-disc space-y-1 pl-5">
              {reasonMessages.map(message => <li key={message}>{message}</li>)}
            </ul>
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
