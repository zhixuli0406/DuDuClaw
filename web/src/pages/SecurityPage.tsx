import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type AuditEvent } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import {
  Shield,
  Lock,
  ShieldCheck,
  Users,
  Timer,
  AlertTriangle,
  FileWarning,
  History,
} from 'lucide-react';

interface SecurityStatus {
  credential_proxy: { active: boolean; vault_backend: string; injected_secrets: number };
  mount_guard: { rules: Array<{ path: string; access: string }> };
  rbac: Array<{
    agent_id: string; role: string;
    tool_use: boolean; web_access: boolean;
    file_write: boolean; shell_exec: boolean; delegate: boolean;
  }>;
  rate_limiter: { requests_per_minute: number; concurrent_requests: number };
  soul_drift: Array<{ agent_id: string; soul_exists: boolean; gvu_enabled: boolean }>;
}

export function SecurityPage() {
  const intl = useIntl();
  const [auditEvents, setAuditEvents] = useState<AuditEvent[]>([]);
  const [auditLoading, setAuditLoading] = useState(false);
  const [status, setStatus] = useState<SecurityStatus | null>(null);
  const connectionState = useConnectionStore((s) => s.state);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setAuditLoading(true);
    api.security
      .auditLog(30)
      .then((res) => setAuditEvents(res?.events ?? []))
      .catch(() => setAuditEvents([]))
      .finally(() => setAuditLoading(false));

    api.security
      .status()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, [connectionState]);

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'security.title' })}
      </h2>

      <div className="grid gap-6 lg:grid-cols-2">
        {/* Audit Log */}
        <SecurityCard
          icon={History}
          title={intl.formatMessage({ id: 'security.audit.title' })}
          description={intl.formatMessage({ id: 'security.audit.desc' })}
        >
          {auditLoading ? (
            <p className="py-4 text-center text-sm text-stone-400">
              {intl.formatMessage({ id: 'common.loading' })}
            </p>
          ) : auditEvents.length === 0 ? (
            <p className="py-4 text-center text-sm text-stone-400">
              {intl.formatMessage({ id: 'security.audit.empty' })}
            </p>
          ) : (
            <div className="max-h-64 space-y-2 overflow-y-auto">
              {auditEvents.map((evt, i) => (
                <AuditRow key={`${evt.timestamp}-${i}`} event={evt} />
              ))}
            </div>
          )}
        </SecurityCard>

        {/* Credential Proxy */}
        <SecurityCard
          icon={Lock}
          title="Credential Proxy"
          description="Credential injection via secure proxy. No secrets exposed to agents."
        >
          <div className="space-y-3">
            <StatusRow
              label="Proxy Status"
              status={status?.credential_proxy.active ? 'active' : 'inactive'}
            />
            <StatusRow
              label="Vault Backend"
              value={status?.credential_proxy.vault_backend ?? '—'}
            />
            <StatusRow
              label="Injected Secrets"
              value={String(status?.credential_proxy.injected_secrets ?? 0)}
            />
          </div>
        </SecurityCard>

        {/* Mount Guard */}
        <SecurityCard
          icon={ShieldCheck}
          title="Mount Guard"
          description="Container filesystem mount rules. Controls what agents can access."
        >
          <div className="space-y-2">
            {status?.mount_guard.rules && status.mount_guard.rules.length > 0 ? (
              status.mount_guard.rules.map((rule) => (
                <RuleRow key={rule.path} path={rule.path} access={rule.access} />
              ))
            ) : (
              <p className="py-2 text-center text-sm text-stone-400">
                {intl.formatMessage({ id: 'common.noData' })}
              </p>
            )}
          </div>
        </SecurityCard>

        {/* RBAC */}
        <SecurityCard
          icon={Users}
          title="RBAC"
          description="Role-based access control for agent permissions."
        >
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-stone-200 dark:border-stone-700">
                  <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                    Agent
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    Tool
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    Web
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    File
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    Shell
                  </th>
                </tr>
              </thead>
              <tbody className="text-stone-700 dark:text-stone-300">
                {(status?.rbac ?? []).map((agent) => (
                  <tr key={agent.agent_id} className="border-b border-stone-100 dark:border-stone-800">
                    <td className="py-2 text-stone-700 dark:text-stone-300">
                      <code className="text-xs">{agent.agent_id}</code>
                      <span className="ml-1 text-xs text-stone-400">({agent.role})</span>
                    </td>
                    <PermCell allowed={agent.tool_use} />
                    <PermCell allowed={agent.web_access} />
                    <PermCell allowed={agent.file_write} />
                    <PermCell allowed={agent.shell_exec} />
                  </tr>
                ))}
                {(!status?.rbac || status.rbac.length === 0) && (
                  <tr>
                    <td colSpan={5} className="py-4 text-center text-sm text-stone-400">
                      {intl.formatMessage({ id: 'common.noData' })}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </SecurityCard>

        {/* Rate Limiter */}
        <SecurityCard
          icon={Timer}
          title="Rate Limiter"
          description="API call rate limiting per agent per time window."
        >
          <div className="space-y-3">
            <LimitRow
              label="Requests / minute"
              value={String(status?.rate_limiter.requests_per_minute ?? 60)}
            />
            <LimitRow
              label="Concurrent requests"
              value={String(status?.rate_limiter.concurrent_requests ?? 5)}
            />
          </div>
        </SecurityCard>
      </div>
    </div>
  );
}

function PermCell({ allowed }: { allowed: boolean }) {
  return (
    <td className="py-2 text-center">
      {allowed ? (
        <span className="text-emerald-500">&#10003;</span>
      ) : (
        <span className="text-stone-300 dark:text-stone-600">&#10005;</span>
      )}
    </td>
  );
}

function AuditRow({ event }: { event: AuditEvent }) {
  const severityStyles: Record<string, string> = {
    info: 'text-blue-500',
    warning: 'text-amber-500',
    critical: 'text-rose-500',
  };

  const SevIcon = event.severity === 'critical' ? AlertTriangle
    : event.severity === 'warning' ? FileWarning
    : Shield;

  const time = new Date(event.timestamp).toLocaleString();

  return (
    <div className="flex items-start gap-2 rounded-lg bg-stone-50 p-2.5 dark:bg-stone-800/50">
      <SevIcon className={`mt-0.5 h-4 w-4 shrink-0 ${severityStyles[event.severity] ?? 'text-stone-400'}`} />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-stone-900 dark:text-stone-100">
            {event.event_type}
          </span>
          <span className="text-xs text-stone-400">{event.agent_id}</span>
        </div>
        <p className="truncate text-xs text-stone-500 dark:text-stone-400">
          {time}
        </p>
      </div>
    </div>
  );
}

function SecurityCard({
  icon: Icon,
  title,
  description,
  children,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="mb-4 flex items-center gap-3">
        <div className="rounded-lg bg-amber-100 p-2.5 dark:bg-amber-900/30">
          <Icon className="h-5 w-5 text-amber-600 dark:text-amber-400" />
        </div>
        <div>
          <h3 className="font-semibold text-stone-900 dark:text-stone-50">
            {title}
          </h3>
          <p className="text-xs text-stone-500 dark:text-stone-400">
            {description}
          </p>
        </div>
      </div>
      {children}
    </div>
  );
}

function StatusRow({
  label,
  status,
  value,
}: {
  label: string;
  status?: string;
  value?: string;
}) {
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-stone-600 dark:text-stone-400">{label}</span>
      {status === 'active' ? (
        <span className="inline-flex items-center gap-1 rounded-full bg-emerald-100 px-2.5 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400">
          <Shield className="h-3 w-3" />
          Active
        </span>
      ) : status === 'inactive' ? (
        <span className="inline-flex items-center gap-1 rounded-full bg-stone-100 px-2.5 py-0.5 text-xs font-medium text-stone-500 dark:bg-stone-800 dark:text-stone-400">
          Inactive
        </span>
      ) : (
        <span className="font-medium text-stone-900 dark:text-stone-50">
          {value}
        </span>
      )}
    </div>
  );
}

function RuleRow({ path, access }: { path: string; access: string }) {
  const accessStyles: Record<string, string> = {
    rw: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
    ro: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
    deny: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
  };

  return (
    <div className="flex items-center justify-between text-sm">
      <code className="rounded bg-stone-100 px-2 py-0.5 font-mono text-xs text-stone-700 dark:bg-stone-800 dark:text-stone-300">
        {path}
      </code>
      <span
        className={`inline-flex rounded-full px-2 py-0.5 text-xs font-medium ${accessStyles[access] ?? ''}`}
      >
        {access}
      </span>
    </div>
  );
}

function LimitRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-stone-600 dark:text-stone-400">{label}</span>
      <span className="font-medium text-stone-900 dark:text-stone-50">
        {value}
      </span>
    </div>
  );
}
