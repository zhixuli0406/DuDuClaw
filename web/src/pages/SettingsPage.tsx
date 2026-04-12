import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
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
  Plus,
  Wrench,
  Download,
  ArrowUpCircle,
  RefreshCw,
  Mic,
  Zap,
} from 'lucide-react';

type TabId = 'general' | 'container' | 'heartbeat' | 'cron' | 'voice' | 'proactive' | 'doctor' | 'update';

export function SettingsPage() {
  const intl = useIntl();
  const [searchParams] = useSearchParams();
  const initialTab = (searchParams.get('tab') as TabId) || 'general';
  const [activeTab, setActiveTab] = useState<TabId>(initialTab);

  const tabs: ReadonlyArray<{ id: TabId; label: string; icon: React.ComponentType<{ className?: string }> }> = [
    { id: 'general', label: intl.formatMessage({ id: 'settings.general' }), icon: Settings },
    { id: 'container', label: intl.formatMessage({ id: 'settings.container' }), icon: Container },
    { id: 'heartbeat', label: intl.formatMessage({ id: 'settings.heartbeat' }), icon: HeartPulse },
    { id: 'cron', label: intl.formatMessage({ id: 'settings.cron' }), icon: Clock },
    { id: 'voice', label: intl.formatMessage({ id: 'settings.voice' }), icon: Mic },
    { id: 'proactive', label: intl.formatMessage({ id: 'settings.proactive' }), icon: Zap },
    { id: 'doctor', label: intl.formatMessage({ id: 'settings.doctor' }), icon: Stethoscope },
    { id: 'update', label: intl.formatMessage({ id: 'settings.update' }), icon: Download },
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
      {activeTab === 'voice' && <VoiceTab />}
      {activeTab === 'proactive' && <ProactiveTab />}
      {activeTab === 'doctor' && <DoctorTab />}
      {activeTab === 'update' && <UpdateTab />}
    </div>
  );
}

function GeneralTab() {
  const intl = useIntl();
  const { status } = useSystemStore();
  const [logLevel, setLogLevel] = useState('info');
  const [rotationStrategy, setRotationStrategy] = useState('priority');
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  // Load current config on mount
  useEffect(() => {
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw === 'string') {
        // Parse TOML string for current values
        const logMatch = raw.match(/level\s*=\s*"(\w+)"/);
        if (logMatch) setLogLevel(logMatch[1]);
        const rotMatch = raw.match(/strategy\s*=\s*"(\w+)"/);
        if (rotMatch) setRotationStrategy(rotMatch[1]);
      }
    }).catch(() => {});
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    try {
      await api.system.updateConfig({ log_level: logLevel, rotation_strategy: rotationStrategy });
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch {
      // error handled silently
    } finally {
      setSaving(false);
    }
  };

  const selectStyle = 'rounded-lg border border-stone-300 bg-white px-3 py-1.5 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50';

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

        {/* Editable: Log Level */}
        <div className="flex items-center justify-between border-b border-stone-100 pb-3 dark:border-stone-800">
          <span className="text-sm text-stone-600 dark:text-stone-400">
            {intl.formatMessage({ id: 'settings.general.logLevel' })}
          </span>
          <select value={logLevel} onChange={(e) => setLogLevel(e.target.value)} className={selectStyle}>
            {['trace', 'debug', 'info', 'warn', 'error'].map((l) => (
              <option key={l} value={l}>{l}</option>
            ))}
          </select>
        </div>

        {/* Editable: Rotation Strategy */}
        <div className="flex items-center justify-between border-b border-stone-100 pb-3 last:border-0 dark:border-stone-800">
          <span className="text-sm text-stone-600 dark:text-stone-400">
            {intl.formatMessage({ id: 'settings.general.rotationStrategy' })}
          </span>
          <select value={rotationStrategy} onChange={(e) => setRotationStrategy(e.target.value)} className={selectStyle}>
            <option value="priority">Priority</option>
            <option value="round_robin">Round Robin</option>
            <option value="least_cost">Least Cost</option>
            <option value="failover">Failover</option>
          </select>
        </div>

        {/* Save button */}
        <div className="flex justify-end gap-2 pt-2">
          {saved && (
            <span className="self-center text-xs text-emerald-600 dark:text-emerald-400">
              {intl.formatMessage({ id: 'settings.general.saved' })}
            </span>
          )}
          <button onClick={handleSave} disabled={saving} className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50">
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
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
  const [heartbeats, setHeartbeats] = useState<
    ReadonlyArray<{
      agent_id: string;
      enabled: boolean;
      interval_seconds: number;
      cron: string;
      last_run?: string;
      next_run?: string;
      total_runs: number;
      active_runs: number;
      max_concurrent: number;
    }>
  >([]);

  useEffect(() => {
    api.heartbeat
      .status()
      .then((r) => setHeartbeats(r?.heartbeats ?? []))
      .catch(() => {});
    const interval = setInterval(() => {
      api.heartbeat
        .status()
        .then((r) => setHeartbeats(r?.heartbeats ?? []))
        .catch(() => {});
    }, 15_000);
    return () => clearInterval(interval);
  }, []);

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3 mb-4">
        <HeartPulse className="h-5 w-5 text-amber-600 dark:text-amber-400" />
        <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'settings.heartbeat' })}
        </h3>
      </div>

      {heartbeats.length === 0 ? (
        <p className="py-8 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'common.noData' })}
        </p>
      ) : (
        <div className="space-y-3">
          {heartbeats.map((hb) => (
            <div
              key={hb.agent_id}
              className="flex items-center justify-between rounded-lg bg-stone-50 p-3 dark:bg-stone-800/50"
            >
              <div>
                <span className="text-sm font-medium text-stone-900 dark:text-stone-100">
                  {hb.agent_id}
                </span>
                <div className="flex gap-3 text-xs text-stone-400 mt-0.5">
                  <span>{hb.cron || `${hb.interval_seconds}s`}</span>
                  <span>Runs: {hb.total_runs}</span>
                  {hb.last_run && (
                    <span>Last: {new Date(hb.last_run).toLocaleTimeString()}</span>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-xs text-stone-400">
                  {hb.active_runs}/{hb.max_concurrent}
                </span>
                <button
                  onClick={() => {
                    api.agents.update(hb.agent_id, { heartbeat_enabled: !hb.enabled }).then(() => {
                      setHeartbeats((prev) =>
                        prev.map((h) => h.agent_id === hb.agent_id ? { ...h, enabled: !h.enabled } : h)
                      );
                    }).catch(() => {});
                  }}
                  title={intl.formatMessage({ id: 'settings.heartbeat.toggle' })}
                  className={cn(
                    'inline-block h-2.5 w-2.5 rounded-full cursor-pointer ring-2 ring-offset-1 ring-transparent hover:ring-amber-400 transition-all',
                    hb.enabled ? 'bg-emerald-500' : 'bg-stone-300 dark:bg-stone-600'
                  )}
                />
                <button
                  onClick={() => api.heartbeat.trigger(hb.agent_id).catch(() => {})}
                  className="rounded px-1.5 py-0.5 text-xs text-amber-600 hover:bg-amber-50 dark:text-amber-400 dark:hover:bg-amber-900/20"
                >
                  <Play className="h-3 w-3" />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function CronTab() {
  const intl = useIntl();
  const [tasks, setTasks] = useState<
    ReadonlyArray<{ id?: string; name?: string; agent_id: string; cron?: string; schedule?: string; enabled: boolean }>
  >([]);
  const [showAdd, setShowAdd] = useState(false);
  const [newName, setNewName] = useState('');
  const [newSchedule, setNewSchedule] = useState('0 * * * *');
  const [newAgent, setNewAgent] = useState('');
  const [adding, setAdding] = useState(false);

  const fetchTasks = useCallback(async () => {
    try {
      const result = await api.cron.list();
      setTasks(result?.tasks ?? []);
    } catch {
      // error handled silently
    }
  }, []);

  useEffect(() => {
    fetchTasks();
  }, [fetchTasks]);

  const handleAdd = async () => {
    if (!newName.trim()) return;
    setAdding(true);
    try {
      await api.cron.add(newAgent, newSchedule, newName.trim());
      setShowAdd(false);
      setNewName('');
      setNewSchedule('0 * * * *');
      setNewAgent('');
      await fetchTasks();
    } catch {
      // error
    } finally {
      setAdding(false);
    }
  };

  const handlePause = async (name: string) => {
    try {
      await api.cron.pause(name);
      await fetchTasks();
    } catch { /* ignore */ }
  };

  const handleRemove = async (name: string) => {
    try {
      await api.cron.remove(name);
      await fetchTasks();
    } catch { /* ignore */ }
  };

  const inputStyle = 'rounded-lg border border-stone-300 bg-white px-3 py-1.5 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50';

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <Clock className="h-5 w-5 text-amber-600 dark:text-amber-400" />
          <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'settings.cron' })}
          </h3>
        </div>
        <button
          onClick={() => setShowAdd(!showAdd)}
          className="inline-flex items-center gap-1 rounded-lg bg-amber-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-amber-600"
        >
          <Plus className="h-3.5 w-3.5" />
          {intl.formatMessage({ id: 'settings.cron.add' })}
        </button>
      </div>

      {/* Add task form */}
      {showAdd && (
        <div className="mb-4 rounded-lg border border-amber-200 bg-amber-50/50 p-4 dark:border-amber-800 dark:bg-amber-900/10">
          <div className="grid gap-3 sm:grid-cols-3">
            <input type="text" placeholder={intl.formatMessage({ id: 'settings.cron.name' })} value={newName} onChange={(e) => setNewName(e.target.value)} className={inputStyle} />
            <input type="text" placeholder="0 * * * *" value={newSchedule} onChange={(e) => setNewSchedule(e.target.value)} className={inputStyle} />
            <input type="text" placeholder={intl.formatMessage({ id: 'settings.cron.agent' })} value={newAgent} onChange={(e) => setNewAgent(e.target.value)} className={inputStyle} />
          </div>
          <div className="mt-3 flex justify-end gap-2">
            <button onClick={() => setShowAdd(false)} className="rounded-lg border border-stone-300 px-3 py-1.5 text-xs text-stone-600 dark:border-stone-600 dark:text-stone-400">
              {intl.formatMessage({ id: 'common.cancel' })}
            </button>
            <button onClick={handleAdd} disabled={adding || !newName.trim()} className="rounded-lg bg-amber-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-amber-600 disabled:opacity-50">
              {adding ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
            </button>
          </div>
        </div>
      )}

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
                  {intl.formatMessage({ id: 'settings.cron.name' })}
                </th>
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'settings.cron.agent' })}
                </th>
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'settings.cron.schedule' })}
                </th>
                <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'settings.cron.enabled' })}
                </th>
                <th className="py-2 text-right font-medium text-stone-500 dark:text-stone-400" />
              </tr>
            </thead>
            <tbody>
              {tasks.map((task) => {
                const taskName = task.name ?? task.id ?? '';
                const taskCron = task.schedule ?? task.cron ?? '';
                return (
                  <tr
                    key={taskName}
                    className="border-b border-stone-100 dark:border-stone-800"
                  >
                    <td className="py-2 font-mono text-xs text-stone-700 dark:text-stone-300">
                      {taskName}
                    </td>
                    <td className="py-2 text-stone-700 dark:text-stone-300">
                      {task.agent_id}
                    </td>
                    <td className="py-2">
                      <code className="rounded bg-stone-100 px-2 py-0.5 font-mono text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                        {taskCron}
                      </code>
                    </td>
                    <td className="py-2 text-center">
                      {task.enabled ? (
                        <span className="inline-flex items-center rounded-full bg-emerald-100 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400">
                          {intl.formatMessage({ id: 'settings.cron.enabled' })}
                        </span>
                      ) : (
                        <span className="inline-flex items-center rounded-full bg-stone-100 px-2 py-0.5 text-xs font-medium text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                          Disabled
                        </span>
                      )}
                    </td>
                    <td className="py-2 text-right">
                      <div className="flex justify-end gap-1">
                        {task.enabled && (
                          <button
                            onClick={() => handlePause(taskName)}
                            className="rounded px-2 py-1 text-xs text-amber-600 hover:bg-amber-50 dark:text-amber-400 dark:hover:bg-amber-900/20"
                          >
                            {intl.formatMessage({ id: 'settings.cron.pause' })}
                          </button>
                        )}
                        <button
                          onClick={() => handleRemove(taskName)}
                          className="rounded px-2 py-1 text-xs text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-900/20"
                        >
                          {intl.formatMessage({ id: 'settings.cron.remove' })}
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              })}
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

function UpdateTab() {
  const intl = useIntl();
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState('');
  const [installed, setInstalled] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<{
    available: boolean;
    current_version: string;
    latest_version: string;
    release_notes: string;
    published_at: string;
    download_url: string;
    install_method: string;
    brew_formula?: string;
  } | null>(null);

  // [H1] useRef guard prevents double-click race — declared before handleCheck
  const installingRef = useRef(false);

  const handleCheck = useCallback(async () => {
    if (installingRef.current) return; // [R2:NM4] block check during install
    setChecking(true);
    setError('');
    setInstalled(false);
    setUpdateInfo(null); // [R2:NL3] clear stale data immediately
    try {
      const info = await api.system.checkUpdate();
      setUpdateInfo(info);
    } catch {
      setError(intl.formatMessage({ id: 'settings.update.failed' }));
    } finally {
      setChecking(false);
    }
  }, [intl]);

  // [M2] applyUpdate no longer sends URL — server uses cached URL from check_update
  const handleInstall = async () => {
    if (installingRef.current || !updateInfo?.download_url) return;
    installingRef.current = true;
    setInstalling(true);
    setError('');
    try {
      const result = await api.system.applyUpdate();
      if (result.success) {
        setInstalled(true);
      } else {
        setError(result.message);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : '';
      setError(`${intl.formatMessage({ id: 'settings.update.failed' })}${msg ? `: ${msg}` : ''}`);
    } finally {
      setInstalling(false);
      installingRef.current = false;
    }
  };

  const isHomebrew = updateInfo?.install_method === 'homebrew';
  const noBinary = updateInfo?.available && !updateInfo.download_url;

  return (
    <div className="space-y-4">
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <div className="flex items-center justify-between mb-6">
          <div className="flex items-center gap-3">
            <ArrowUpCircle className="h-5 w-5 text-amber-600 dark:text-amber-400" />
            <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'settings.update' })}
            </h3>
          </div>
          <button
            onClick={handleCheck}
            disabled={checking}
            className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            <RefreshCw className={cn('h-4 w-4', checking && 'animate-spin')} />
            {checking
              ? intl.formatMessage({ id: 'settings.update.checking' })
              : intl.formatMessage({ id: 'settings.update.check' })}
          </button>
        </div>

        {/* Status display */}
        {!updateInfo && !error && (
          <div className="flex flex-col items-center justify-center py-12 text-stone-400 dark:text-stone-500">
            <Download className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
            <p>{intl.formatMessage({ id: 'settings.update.check' })}</p>
          </div>
        )}

        {error && (
          <div className="rounded-lg border border-rose-200 bg-rose-50 p-4 dark:border-rose-800 dark:bg-rose-900/20">
            <div className="flex items-center gap-2">
              <XCircle className="h-5 w-5 text-rose-500" />
              <span className="text-sm text-rose-700 dark:text-rose-400">{error}</span>
            </div>
          </div>
        )}

        {installed && (
          <div className="rounded-lg border border-emerald-200 bg-emerald-50 p-4 dark:border-emerald-800 dark:bg-emerald-900/20">
            <div className="flex items-center gap-2">
              <CheckCircle className="h-5 w-5 text-emerald-500" />
              <span className="text-sm text-emerald-700 dark:text-emerald-400">
                {intl.formatMessage({ id: 'settings.update.installed' })}
              </span>
            </div>
          </div>
        )}

        {updateInfo && !installed && (
          <div className="space-y-4">
            {/* Version info */}
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="rounded-lg bg-stone-50 p-4 dark:bg-stone-800/50">
                <span className="text-xs text-stone-400">
                  {intl.formatMessage({ id: 'settings.update.current' })}
                </span>
                <p className="mt-1 text-lg font-semibold text-stone-900 dark:text-stone-50">
                  v{updateInfo.current_version}
                </p>
              </div>
              <div className={cn(
                'rounded-lg p-4',
                updateInfo.available
                  ? 'bg-amber-50 dark:bg-amber-900/20'
                  : 'bg-emerald-50 dark:bg-emerald-900/20'
              )}>
                <span className="text-xs text-stone-400">
                  {intl.formatMessage({ id: 'settings.update.latest' })}
                </span>
                <p className={cn(
                  'mt-1 text-lg font-semibold',
                  updateInfo.available
                    ? 'text-amber-700 dark:text-amber-400'
                    : 'text-emerald-700 dark:text-emerald-400'
                )}>
                  v{updateInfo.latest_version}
                </p>
              </div>
            </div>

            {!updateInfo.available && (
              <div className="flex items-center gap-2 rounded-lg border border-emerald-200 bg-emerald-50 p-4 dark:border-emerald-800 dark:bg-emerald-900/20">
                <CheckCircle className="h-5 w-5 text-emerald-500" />
                <span className="text-sm text-emerald-700 dark:text-emerald-400">
                  {intl.formatMessage({ id: 'settings.update.upToDate' })}
                </span>
              </div>
            )}

            {updateInfo.available && (
              <>
                {/* Release notes */}
                {updateInfo.release_notes && (
                  <div className="rounded-lg border border-stone-200 p-4 dark:border-stone-700">
                    <h4 className="mb-2 text-sm font-medium text-stone-700 dark:text-stone-300">
                      {intl.formatMessage({ id: 'settings.update.releaseNotes' })}
                    </h4>
                    <pre className="max-h-48 overflow-y-auto whitespace-pre-wrap text-xs text-stone-600 dark:text-stone-400">
                      {updateInfo.release_notes}
                    </pre>
                  </div>
                )}

                {/* Homebrew hint */}
                {isHomebrew && (
                  <div className="rounded-lg border border-amber-200 bg-amber-50 p-4 dark:border-amber-800 dark:bg-amber-900/20">
                    <p className="text-sm text-amber-700 dark:text-amber-400">
                      {intl.formatMessage({ id: 'settings.update.brewHint' })}
                    </p>
                    <code className="mt-2 block rounded bg-stone-800 px-3 py-2 text-sm text-emerald-400">
                      brew upgrade {updateInfo.brew_formula ?? 'duduclaw'}
                    </code>
                  </div>
                )}

                {/* No binary hint */}
                {noBinary && !isHomebrew && (
                  <div className="rounded-lg border border-amber-200 bg-amber-50 p-4 dark:border-amber-800 dark:bg-amber-900/20">
                    <p className="text-sm text-amber-700 dark:text-amber-400">
                      {intl.formatMessage({ id: 'settings.update.noBinary' })}
                    </p>
                  </div>
                )}

                {/* Install button */}
                {!isHomebrew && !noBinary && (
                  <button
                    onClick={handleInstall}
                    disabled={installing}
                    className="inline-flex w-full items-center justify-center gap-2 rounded-lg bg-amber-500 px-4 py-3 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
                  >
                    <Download className={cn('h-4 w-4', installing && 'animate-bounce')} />
                    {installing
                      ? intl.formatMessage({ id: 'settings.update.installing' })
                      : intl.formatMessage({ id: 'settings.update.install' })}
                  </button>
                )}
              </>
            )}
          </div>
        )}
      </div>
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

// ── Voice Settings Tab ─────────────────────────────────────────

function VoiceTab() {
  const intl = useIntl();
  const [config, setConfig] = useState({
    asr_provider: 'auto',
    tts_provider: 'auto',
    asr_language: 'zh',
    tts_voice: '',
    voice_reply_enabled: false,
  });
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.system.updateConfig({ voice: config });
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch { /* ignore */ }
    setSaving(false);
  };

  return (
    <div className="space-y-6 rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'voice.title' })}
      </h3>

      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'voice.asrProvider' })}
          </label>
          <select
            value={config.asr_provider}
            onChange={(e) => setConfig({ ...config, asr_provider: e.target.value })}
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          >
            <option value="auto">Auto</option>
            <option value="whisper-api">{intl.formatMessage({ id: 'voice.provider.whisperApi' })}</option>
            <option value="whisper-local">Whisper Local</option>
            <option value="sensevoice">{intl.formatMessage({ id: 'voice.provider.sensevoice' })}</option>
          </select>
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'voice.ttsProvider' })}
          </label>
          <select
            value={config.tts_provider}
            onChange={(e) => setConfig({ ...config, tts_provider: e.target.value })}
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          >
            <option value="auto">Auto</option>
            <option value="edge-tts">{intl.formatMessage({ id: 'voice.provider.edgeTts' })}</option>
            <option value="minimax">{intl.formatMessage({ id: 'voice.provider.minimax' })}</option>
            <option value="openai-tts">{intl.formatMessage({ id: 'voice.provider.openaiTts' })}</option>
            <option value="piper">{intl.formatMessage({ id: 'voice.provider.piper' })}</option>
          </select>
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'voice.language' })}
          </label>
          <select
            value={config.asr_language}
            onChange={(e) => setConfig({ ...config, asr_language: e.target.value })}
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          >
            <option value="zh">中文 (zh)</option>
            <option value="en">English (en)</option>
            <option value="ja">日本語 (ja)</option>
            <option value="ko">한국어 (ko)</option>
          </select>
        </div>

        <div className="flex items-center gap-3 pt-6">
          <label className="relative inline-flex cursor-pointer items-center">
            <input
              type="checkbox"
              checked={config.voice_reply_enabled}
              onChange={(e) => setConfig({ ...config, voice_reply_enabled: e.target.checked })}
              className="peer sr-only"
            />
            <div className="peer h-6 w-11 rounded-full bg-stone-200 after:absolute after:left-[2px] after:top-[2px] after:h-5 after:w-5 after:rounded-full after:bg-white after:transition-all peer-checked:bg-amber-500 peer-checked:after:translate-x-full dark:bg-stone-600"></div>
          </label>
          <span className="text-sm text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'voice.voiceMode' })}
          </span>
        </div>
      </div>

      <div className="flex justify-end pt-2">
        <button
          onClick={handleSave}
          disabled={saving}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          {saved ? intl.formatMessage({ id: 'settings.general.saved' }) : saving ? '...' : 'Save'}
        </button>
      </div>
    </div>
  );
}

// ── Proactive Settings Tab ─────────────────────────────────────

function ProactiveTab() {
  const intl = useIntl();
  const [config, setConfig] = useState({
    enabled: false,
    check_interval: '*/30 * * * *',
    quiet_hours_start: 23,
    quiet_hours_end: 8,
    max_messages_per_hour: 3,
    notify_channel: '',
    notify_chat_id: '',
  });
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.system.updateConfig({ proactive: config });
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch { /* ignore */ }
    setSaving(false);
  };

  return (
    <div className="space-y-6 rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'proactive.title' })}
      </h3>

      <div className="grid gap-4 sm:grid-cols-2">
        <div className="flex items-center gap-3">
          <label className="relative inline-flex cursor-pointer items-center">
            <input
              type="checkbox"
              checked={config.enabled}
              onChange={(e) => setConfig({ ...config, enabled: e.target.checked })}
              className="peer sr-only"
            />
            <div className="peer h-6 w-11 rounded-full bg-stone-200 after:absolute after:left-[2px] after:top-[2px] after:h-5 after:w-5 after:rounded-full after:bg-white after:transition-all peer-checked:bg-amber-500 peer-checked:after:translate-x-full dark:bg-stone-600"></div>
          </label>
          <span className="text-sm text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: config.enabled ? 'proactive.enabled' : 'proactive.disabled' })}
          </span>
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            Check Interval
          </label>
          <input
            type="text"
            value={config.check_interval}
            onChange={(e) => setConfig({ ...config, check_interval: e.target.value })}
            placeholder="*/30 * * * *"
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          />
          <p className="text-xs text-stone-400">Cron expression</p>
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'proactive.quietHours' })}
          </label>
          <div className="flex items-center gap-2">
            <input type="number" min={0} max={23} value={config.quiet_hours_start}
              onChange={(e) => setConfig({ ...config, quiet_hours_start: +e.target.value })}
              className="w-16 rounded-lg border border-stone-300 bg-white px-2 py-2 text-sm text-center dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50" />
            <span className="text-stone-400">—</span>
            <input type="number" min={0} max={23} value={config.quiet_hours_end}
              onChange={(e) => setConfig({ ...config, quiet_hours_end: +e.target.value })}
              className="w-16 rounded-lg border border-stone-300 bg-white px-2 py-2 text-sm text-center dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50" />
          </div>
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            Max messages / hour
          </label>
          <input type="number" min={1} max={60} value={config.max_messages_per_hour}
            onChange={(e) => setConfig({ ...config, max_messages_per_hour: +e.target.value })}
            className="w-24 rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50" />
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            Notify Channel
          </label>
          <select value={config.notify_channel}
            onChange={(e) => setConfig({ ...config, notify_channel: e.target.value })}
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50">
            <option value="">Select...</option>
            <option value="telegram">Telegram</option>
            <option value="line">LINE</option>
            <option value="discord">Discord</option>
          </select>
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            Chat ID
          </label>
          <input type="text" value={config.notify_chat_id}
            onChange={(e) => setConfig({ ...config, notify_chat_id: e.target.value })}
            placeholder="e.g., 123456789"
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50" />
        </div>
      </div>

      <div className="flex justify-end pt-2">
        <button
          onClick={handleSave}
          disabled={saving}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          {saved ? intl.formatMessage({ id: 'settings.general.saved' }) : saving ? '...' : 'Save'}
        </button>
      </div>
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
