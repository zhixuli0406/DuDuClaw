import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { api } from '@/lib/api';
import {
  Settings,
  Container,
  HeartPulse,
  Clock,
  Stethoscope,
  CheckCircle,
  AlertTriangle,
  XCircle,
  Play,
  Wrench,
} from 'lucide-react';

type TabId = 'general' | 'container' | 'heartbeat' | 'cron' | 'doctor';

export function SettingsPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<TabId>('general');

  const tabs: ReadonlyArray<{ id: TabId; label: string; icon: React.ComponentType<{ className?: string }> }> = [
    { id: 'general', label: intl.formatMessage({ id: 'settings.general' }), icon: Settings },
    { id: 'container', label: intl.formatMessage({ id: 'settings.container' }), icon: Container },
    { id: 'heartbeat', label: intl.formatMessage({ id: 'settings.heartbeat' }), icon: HeartPulse },
    { id: 'cron', label: intl.formatMessage({ id: 'settings.cron' }), icon: Clock },
    { id: 'doctor', label: intl.formatMessage({ id: 'settings.doctor' }), icon: Stethoscope },
  ];

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'settings.title' })}
      </h2>

      {/* Tabs */}
      <div className="flex gap-1 overflow-x-auto rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
        {tabs.map((tab) => {
          const TabIcon = tab.icon;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={cn(
                'flex items-center gap-2 whitespace-nowrap rounded-md px-4 py-2 text-sm font-medium transition-colors',
                activeTab === tab.id
                  ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
                  : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-300'
              )}
            >
              <TabIcon className="h-4 w-4" />
              {tab.label}
            </button>
          );
        })}
      </div>

      {activeTab === 'general' && <GeneralTab />}
      {activeTab === 'container' && <ContainerTab />}
      {activeTab === 'heartbeat' && <HeartbeatTab />}
      {activeTab === 'cron' && <CronTab />}
      {activeTab === 'doctor' && <DoctorTab />}
    </div>
  );
}

function GeneralTab() {
  const intl = useIntl();
  const { status } = useSystemStore();

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'settings.general' })}
      </h3>

      <div className="space-y-4">
        <SettingRow label="Gateway Address" value={status?.gateway_address ?? '0.0.0.0:3100'} />
        <SettingRow label="Version" value={status?.version ?? '-'} />
        <SettingRow
          label="Uptime"
          value={status?.uptime_seconds ? formatUptime(status.uptime_seconds) : '-'}
        />
        <SettingRow label="Log Level" value="info" />
      </div>
    </div>
  );
}

function ContainerTab() {
  const intl = useIntl();

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3 mb-4">
        <Container className="h-5 w-5 text-amber-600 dark:text-amber-400" />
        <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'settings.container' })}
        </h3>
      </div>

      <div className="space-y-4">
        <SettingRow label="Engine" value="Docker" />
        <SettingRow label="Socket" value="/var/run/docker.sock" />
        <SettingRow label="Status" value="Detected" badge="emerald" />
      </div>
    </div>
  );
}

function HeartbeatTab() {
  const intl = useIntl();

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3 mb-4">
        <HeartPulse className="h-5 w-5 text-amber-600 dark:text-amber-400" />
        <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'settings.heartbeat' })}
        </h3>
      </div>

      <div className="space-y-4">
        <SettingRow label="Interval" value="30s" />
        <SettingRow label="Timeout" value="10s" />
        <SettingRow label="Max Retries" value="3" />
        <SettingRow label="Status" value="Enabled" badge="emerald" />
      </div>
    </div>
  );
}

function CronTab() {
  const intl = useIntl();
  const [tasks, setTasks] = useState<
    ReadonlyArray<{ id: string; agent_id: string; cron: string; enabled: boolean }>
  >([]);

  const fetchTasks = useCallback(async () => {
    try {
      const result = await api.cron.list();
      setTasks(result.tasks);
    } catch {
      // error handled silently
    }
  }, []);

  useEffect(() => {
    fetchTasks();
  }, [fetchTasks]);

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <Clock className="h-5 w-5 text-amber-600 dark:text-amber-400" />
          <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'settings.cron' })}
          </h3>
        </div>
      </div>

      {tasks.length === 0 ? (
        <div className="flex items-center justify-center py-12 text-stone-400 dark:text-stone-500">
          <p>{intl.formatMessage({ id: 'common.noData' })}</p>
        </div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-stone-200 dark:border-stone-700">
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  ID
                </th>
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  Agent
                </th>
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  Schedule
                </th>
                <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                  Status
                </th>
              </tr>
            </thead>
            <tbody>
              {tasks.map((task) => (
                <tr
                  key={task.id}
                  className="border-b border-stone-100 dark:border-stone-800"
                >
                  <td className="py-2 font-mono text-xs text-stone-700 dark:text-stone-300">
                    {task.id}
                  </td>
                  <td className="py-2 text-stone-700 dark:text-stone-300">
                    {task.agent_id}
                  </td>
                  <td className="py-2">
                    <code className="rounded bg-stone-100 px-2 py-0.5 font-mono text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                      {task.cron}
                    </code>
                  </td>
                  <td className="py-2 text-center">
                    {task.enabled ? (
                      <span className="inline-flex items-center rounded-full bg-emerald-100 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400">
                        Enabled
                      </span>
                    ) : (
                      <span className="inline-flex items-center rounded-full bg-stone-100 px-2 py-0.5 text-xs font-medium text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                        Disabled
                      </span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function DoctorTab() {
  const intl = useIntl();
  const { doctorChecks, runDoctor, loading } = useSystemStore();

  const statusIcon: Record<string, React.ReactNode> = {
    pass: <CheckCircle className="h-5 w-5 text-emerald-500" />,
    warn: <AlertTriangle className="h-5 w-5 text-amber-500" />,
    fail: <XCircle className="h-5 w-5 text-rose-500" />,
  };

  const statusBg: Record<string, string> = {
    pass: 'border-emerald-200 bg-emerald-50 dark:border-emerald-800 dark:bg-emerald-900/20',
    warn: 'border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20',
    fail: 'border-rose-200 bg-rose-50 dark:border-rose-800 dark:bg-rose-900/20',
  };

  const handleRepair = async () => {
    try {
      await api.system.doctorRepair();
      await runDoctor();
    } catch {
      // error handled silently
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex gap-2">
        <button
          onClick={runDoctor}
          disabled={loading}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          <Play className="h-4 w-4" />
          {intl.formatMessage({ id: 'settings.doctor.run' })}
        </button>
        <button
          onClick={handleRepair}
          disabled={loading}
          className="inline-flex items-center gap-2 rounded-lg border border-stone-200 bg-white px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 disabled:opacity-50 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300 dark:hover:bg-stone-700"
        >
          <Wrench className="h-4 w-4" />
          {intl.formatMessage({ id: 'settings.doctor.repair' })}
        </button>
      </div>

      {doctorChecks.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Stethoscope className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'settings.doctor.run' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2">
          {doctorChecks.map((check) => (
            <div
              key={check.name}
              className={cn(
                'rounded-xl border p-5',
                statusBg[check.status] ?? 'border-stone-200 bg-white'
              )}
            >
              <div className="flex items-start gap-3">
                {statusIcon[check.status]}
                <div className="flex-1">
                  <h4 className="font-semibold text-stone-900 dark:text-stone-50">
                    {check.name}
                  </h4>
                  <p className="mt-1 text-sm text-stone-600 dark:text-stone-400">
                    {check.message}
                  </p>
                  {check.can_repair && check.repair_hint && (
                    <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
                      {check.repair_hint}
                    </p>
                  )}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function SettingRow({
  label,
  value,
  badge,
}: {
  label: string;
  value: string;
  badge?: 'emerald' | 'amber' | 'rose';
}) {
  const badgeStyles: Record<string, string> = {
    emerald:
      'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
    amber:
      'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
    rose: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
  };

  return (
    <div className="flex items-center justify-between border-b border-stone-100 pb-3 last:border-0 last:pb-0 dark:border-stone-800">
      <span className="text-sm text-stone-600 dark:text-stone-400">
        {label}
      </span>
      {badge ? (
        <span
          className={cn(
            'inline-flex rounded-full px-2.5 py-0.5 text-xs font-medium',
            badgeStyles[badge]
          )}
        >
          {value}
        </span>
      ) : (
        <span className="text-sm font-medium text-stone-900 dark:text-stone-50">
          {value}
        </span>
      )}
    </div>
  );
}

function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h ${minutes}m`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}
