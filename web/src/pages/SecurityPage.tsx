import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type AuditEvent } from '@/lib/api';
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

export function SecurityPage() {
  const intl = useIntl();
  const [auditEvents, setAuditEvents] = useState<AuditEvent[]>([]);
  const [auditLoading, setAuditLoading] = useState(false);
  // FE-H1: Security panels below show static placeholder data, not live system state.
  const isStaticData = true;

  useEffect(() => {
    setAuditLoading(true);
    api.security
      .auditLog(30)
      .then((res) => setAuditEvents(res?.events ?? []))
      .catch(() => setAuditEvents([]))
      .finally(() => setAuditLoading(false));
  }, []);

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'security.title' })}
      </h2>

      {isStaticData && (
        <div className="flex items-center gap-2 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900/50 dark:bg-amber-900/20 dark:text-amber-300">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          <span>{intl.formatMessage({ id: 'security.static.banner' })}</span>
        </div>
      )}

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
            <StatusRow label="Proxy Status" status="active" />
            <StatusRow label="Vault Backend" value="env" />
            <StatusRow label="Injected Secrets" value="3" />
          </div>
        </SecurityCard>

        {/* Mount Guard */}
        <SecurityCard
          icon={ShieldCheck}
          title="Mount Guard"
          description="Container filesystem mount rules. Controls what agents can access."
        >
          <div className="space-y-2">
            <RuleRow path="/workspace" access="rw" />
            <RuleRow path="/tmp" access="rw" />
            <RuleRow path="/etc" access="ro" />
            <RuleRow path="/var/run/docker.sock" access="deny" />
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
                    Permission
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    Main
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    Specialist
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    Worker
                  </th>
                </tr>
              </thead>
              <tbody className="text-stone-700 dark:text-stone-300">
                <PermRow perm="tool_use" main warn={false} specialist worker={false} />
                <PermRow perm="web_access" main warn={false} specialist={false} worker={false} />
                <PermRow perm="file_write" main warn={false} specialist worker />
                <PermRow perm="shell_exec" main warn specialist={false} worker={false} />
                <PermRow perm="delegate" main warn={false} specialist={false} worker={false} />
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
            <LimitRow label="Requests / minute" value="60" />
            <LimitRow label="Tokens / minute" value="100,000" />
            <LimitRow label="Concurrent requests" value="5" />
            <LimitRow label="Burst allowance" value="10" />
          </div>
        </SecurityCard>
      </div>
    </div>
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

function PermRow({
  perm,
  main,
  specialist,
  worker,
  warn = false,
}: {
  perm: string;
  main: boolean;
  specialist: boolean;
  worker: boolean;
  warn?: boolean;
}) {
  const renderCell = (allowed: boolean) => (
    <td className="py-2 text-center">
      {allowed ? (
        <span className="text-emerald-500">&#10003;</span>
      ) : (
        <span className="text-stone-300 dark:text-stone-600">&#10005;</span>
      )}
    </td>
  );

  return (
    <tr className="border-b border-stone-100 dark:border-stone-800">
      <td className="py-2 text-stone-700 dark:text-stone-300">
        <code className="text-xs">{perm}</code>
        {warn && (
          <span className="ml-1 text-xs text-amber-500" title="Requires approval">
            &#9888;
          </span>
        )}
      </td>
      {renderCell(main)}
      {renderCell(specialist)}
      {renderCell(worker)}
    </tr>
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
