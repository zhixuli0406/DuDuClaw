import { AlertTriangle } from 'lucide-react';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { Button } from '@/components/mds';

const COMPONENTS = ['launch_service', 'sandbox', 'network', 'storage', 'parse_worker',
  'update_service', 'mcp_boundary', 'runtime_adapters'] as const;

/** Readiness diagnosis only; this component does not authorize execution. */
export function IsolationReadinessBanner() {
  const intl = useIntl();
  const readiness = useSystemStore(s => s.status?.isolation_readiness);
  const loading = useSystemStore(s => s.loading);
  const refresh = useSystemStore(s => s.fetchStatus);
  const admin = useAuthStore(s => s.user?.role === 'admin');
  if (!readiness?.required || readiness.ready) return null;

  return (
    <div role="alert" className="rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
        <div className="min-w-0 flex-1 space-y-1">
          <p className="font-medium">{intl.formatMessage({ id: 'isolationReadiness.title' })}</p>
          <p className="text-muted-foreground">{intl.formatMessage({ id: 'isolationReadiness.description' })}</p>
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
