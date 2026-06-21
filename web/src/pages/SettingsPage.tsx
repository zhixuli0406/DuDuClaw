import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { useAgentsStore } from '@/stores/agents-store';
import {
  api,
  type AutopilotRule,
  type AutopilotHistoryEntry,
  type RedactionConfig,
  type RedactionSourceMode,
  type RedactionSources,
  type RedactionRestoreArgs,
  type RedactionEgressRule,
  type RedactionUpdate,
  type SkillSynthesisConfig,
} from '@/lib/api';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import { ToolApprovalPanel } from '@/components/ToolApprovalPanel';
import { SessionReplayPanel } from '@/components/SessionReplayPanel';
import { BrowserAuditPanel } from '@/components/BrowserAuditPanel';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Tabs,
  Button,
  Badge,
  EmptyState,
  Field,
  controlClass,
} from '@/components/ui';
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
  Workflow,
  Globe,
  Pencil,
  EyeOff,
  Trash2,
  Server,
  Sparkles,
  ExternalLink,
} from 'lucide-react';

type TabId = 'general' | 'system' | 'container' | 'heartbeat' | 'cron' | 'voice' | 'proactive' | 'autopilot' | 'skillSynthesis' | 'redaction' | 'doctor' | 'update' | 'browser';

export function SettingsPage() {
  const intl = useIntl();
  const [searchParams] = useSearchParams();
  const initialTab = (searchParams.get('tab') as TabId) || 'general';
  const [activeTab, setActiveTab] = useState<TabId>(initialTab);

  const tabs: ReadonlyArray<{ id: TabId; label: string; icon: React.ComponentType<{ className?: string }> }> = [
    { id: 'general', label: intl.formatMessage({ id: 'settings.general' }), icon: Settings },
    { id: 'system', label: intl.formatMessage({ id: 'settings.system' }), icon: Server },
    { id: 'container', label: intl.formatMessage({ id: 'settings.container' }), icon: Container },
    { id: 'heartbeat', label: intl.formatMessage({ id: 'settings.heartbeat' }), icon: HeartPulse },
    { id: 'cron', label: intl.formatMessage({ id: 'settings.cron' }), icon: Clock },
    { id: 'voice', label: intl.formatMessage({ id: 'settings.voice' }), icon: Mic },
    { id: 'proactive', label: intl.formatMessage({ id: 'settings.proactive' }), icon: Zap },
    { id: 'autopilot', label: intl.formatMessage({ id: 'settings.autopilot' }), icon: Workflow },
    { id: 'skillSynthesis', label: intl.formatMessage({ id: 'settings.skillSynthesis' }), icon: Sparkles },
    { id: 'redaction', label: intl.formatMessage({ id: 'settings.redaction' }), icon: EyeOff },
    { id: 'doctor', label: intl.formatMessage({ id: 'settings.doctor' }), icon: Stethoscope },
    { id: 'update', label: intl.formatMessage({ id: 'settings.update' }), icon: Download },
    { id: 'browser', label: intl.formatMessage({ id: 'settings.browser' }), icon: Globe },
  ];

  return (
    <Page wide>
      <PageHeader
        icon={Settings}
        title={intl.formatMessage({ id: 'nav.settings' })}
        subtitle={intl.formatMessage({ id: 'settings.title' })}
      />

      <Tabs
        items={tabs}
        value={activeTab}
        onChange={(id) => setActiveTab(id as TabId)}
      />

      {activeTab === 'general' && <GeneralTab />}
      {activeTab === 'system' && <SystemTab />}
      {activeTab === 'container' && <ContainerTab />}
      {activeTab === 'heartbeat' && <HeartbeatTab />}
      {activeTab === 'cron' && <CronTab />}
      {activeTab === 'voice' && <VoiceTab />}
      {activeTab === 'proactive' && <ProactiveTab />}
      {activeTab === 'autopilot' && <AutopilotTab />}
      {activeTab === 'skillSynthesis' && <SkillSynthesisTab />}
      {activeTab === 'redaction' && <RedactionTab />}
      {activeTab === 'doctor' && <DoctorTab />}
      {activeTab === 'update' && <UpdateTab />}
      {activeTab === 'browser' && <BrowserTab />}
    </Page>
  );
}

function GeneralTab() {
  const intl = useIntl();
  const { status } = useSystemStore();
  const [logLevel, setLogLevel] = useState('info');
  const [rotationStrategy, setRotationStrategy] = useState('priority');
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

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
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    try {
      await api.system.updateConfig({ log_level: logLevel, rotation_strategy: rotationStrategy });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const selectStyle = cn(controlClass, 'w-auto min-w-[10rem]');

  return (
    <Card title={intl.formatMessage({ id: 'settings.general' })}>
      <div className="space-y-4">
        <SettingRow label="Gateway Address" value={status?.gateway_address ?? '0.0.0.0:3100'} />
        <SettingRow label="Version" value={status?.version ?? '-'} />
        <SettingRow
          label="Uptime"
          value={status?.uptime_seconds ? formatUptime(status.uptime_seconds) : '-'}
        />

        {/* Editable: Log Level */}
        <div className="flex items-center justify-between border-b border-[var(--panel-border)] pb-3">
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
        <div className="flex items-center justify-between border-b border-[var(--panel-border)] pb-3 last:border-0">
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
        <div className="flex items-center justify-end gap-2 pt-2">
          {saved && (
            <span className="text-xs text-emerald-600 dark:text-emerald-400">
              {intl.formatMessage({ id: 'settings.general.saved' })}
            </span>
          )}
          <Button variant="primary" onClick={handleSave} disabled={saving}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </div>
    </Card>
  );
}

// ── G — System tab (gateway / rotation / general / logging / secret_manager) ──

function SystemTab() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  // [gateway] — bind/port require restart; auth_token is write-only.
  const [bind, setBind] = useState('');
  const [port, setPort] = useState('');
  const [authToken, setAuthToken] = useState('');
  // [rotation]
  const [healthInterval, setHealthInterval] = useState('');
  const [cooldown, setCooldown] = useState('');
  // [general]
  const [defaultAgent, setDefaultAgent] = useState('');
  const [inferenceMode, setInferenceMode] = useState('claude');
  // [logging]
  const [logFormat, setLogFormat] = useState('pretty');
  // [secret_manager]
  const [smBackend, setSmBackend] = useState('config');
  const [vaultAddr, setVaultAddr] = useState('');
  const [vaultMount, setVaultMount] = useState('');
  const [vaultToken, setVaultToken] = useState('');

  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);

  // Load non-secret current values from the TOML config string. Secrets
  // (auth_token / vault_token) are write-only — left blank, only sent if typed.
  useEffect(() => {
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw !== 'string') return;
      const m = (re: RegExp) => raw.match(re)?.[1];
      setBind(m(/\bbind\s*=\s*"([^"]*)"/) ?? '');
      setPort(m(/\bport\s*=\s*(\d+)/) ?? '');
      setHealthInterval(m(/health_check_interval_seconds\s*=\s*(\d+)/) ?? '');
      setCooldown(m(/cooldown_after_rate_limit_seconds\s*=\s*(\d+)/) ?? '');
      setDefaultAgent(m(/default_agent\s*=\s*"([^"]*)"/) ?? '');
      setInferenceMode(m(/inference_mode\s*=\s*"(\w+)"/) ?? 'claude');
      setLogFormat(m(/\bformat\s*=\s*"(\w+)"/) ?? 'pretty');
      setSmBackend(m(/\bbackend\s*=\s*"(\w+)"/) ?? 'config');
      setVaultAddr(m(/vault_addr\s*=\s*"([^"]*)"/) ?? '');
      setVaultMount(m(/vault_mount\s*=\s*"([^"]*)"/) ?? '');
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    try {
      const payload: Record<string, unknown> = {};
      if (bind.trim() !== '') payload.bind = bind.trim();
      if (port.trim() !== '') payload.port = Number(port);
      if (authToken.trim() !== '') payload.auth_token = authToken.trim();
      if (healthInterval.trim() !== '') payload.health_check_interval_seconds = Number(healthInterval);
      if (cooldown.trim() !== '') payload.cooldown_after_rate_limit_seconds = Number(cooldown);
      payload.default_agent = defaultAgent;
      payload.inference_mode = inferenceMode;
      payload.log_format = logFormat;
      const sm: Record<string, unknown> = { backend: smBackend, vault_addr: vaultAddr, vault_mount: vaultMount };
      if (vaultToken.trim() !== '') sm.vault_token = vaultToken.trim();
      payload.secret_manager = sm;

      await api.system.updateConfig(payload);
      setAuthToken('');
      setVaultToken('');
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-6">
      {/* Gateway */}
      <Card title={intl.formatMessage({ id: 'settings.system.gateway' })} bodyClassName="space-y-4">
        <div className="flex items-start gap-2 rounded-lg bg-amber-500/10 p-3 text-xs text-amber-700 ring-1 ring-inset ring-amber-500/20 dark:text-amber-400">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{intl.formatMessage({ id: 'settings.system.restartNote' })}</span>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <FormField label={intl.formatMessage({ id: 'settings.system.bind' })}>
            <input type="text" value={bind} onChange={(e) => setBind(e.target.value)} placeholder="0.0.0.0" className={inputClass} />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'settings.system.port' })}>
            <input type="number" min={1} max={65535} value={port} onChange={(e) => setPort(e.target.value)} placeholder="3100" className={inputClass} />
          </FormField>
        </div>
        <FormField label={intl.formatMessage({ id: 'settings.system.authToken' })} hint={intl.formatMessage({ id: 'settings.system.writeOnly' })}>
          <input type="password" value={authToken} onChange={(e) => setAuthToken(e.target.value)} placeholder="••••••••" className={inputClass} autoComplete="off" />
        </FormField>
      </Card>

      {/* Rotation */}
      <Card title={intl.formatMessage({ id: 'settings.system.rotation' })} bodyClassName="space-y-4">
        <div className="grid grid-cols-2 gap-3">
          <FormField label={intl.formatMessage({ id: 'settings.system.healthInterval' })}>
            <input type="number" min={1} max={86400} value={healthInterval} onChange={(e) => setHealthInterval(e.target.value)} placeholder="60" className={inputClass} />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'settings.system.cooldown' })}>
            <input type="number" min={1} max={86400} value={cooldown} onChange={(e) => setCooldown(e.target.value)} placeholder="120" className={inputClass} />
          </FormField>
        </div>
      </Card>

      {/* General + Logging */}
      <Card title={intl.formatMessage({ id: 'settings.system.general' })} bodyClassName="space-y-4">
        <FormField label={intl.formatMessage({ id: 'settings.system.defaultAgent' })}>
          <select value={defaultAgent} onChange={(e) => setDefaultAgent(e.target.value)} className={selectClass}>
            <option value="">{intl.formatMessage({ id: 'settings.system.none' })}</option>
            {agents.map((a) => (
              <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
            ))}
          </select>
        </FormField>
        <div className="grid grid-cols-2 gap-3">
          <FormField label={intl.formatMessage({ id: 'settings.system.inferenceMode' })}>
            <select value={inferenceMode} onChange={(e) => setInferenceMode(e.target.value)} className={selectClass}>
              <option value="local">local</option>
              <option value="claude">claude</option>
              <option value="hybrid">hybrid</option>
            </select>
          </FormField>
          <FormField label={intl.formatMessage({ id: 'settings.system.logFormat' })}>
            <select value={logFormat} onChange={(e) => setLogFormat(e.target.value)} className={selectClass}>
              <option value="pretty">pretty</option>
              <option value="json">json</option>
            </select>
          </FormField>
        </div>
      </Card>

      {/* Secret manager */}
      <Card title={intl.formatMessage({ id: 'settings.system.secrets' })} bodyClassName="space-y-4">
        <FormField label={intl.formatMessage({ id: 'settings.system.smBackend' })}>
          <select value={smBackend} onChange={(e) => setSmBackend(e.target.value)} className={selectClass}>
            <option value="env">env</option>
            <option value="vault">vault</option>
            <option value="config">config</option>
            <option value="keychain">keychain</option>
          </select>
        </FormField>
        {smBackend === 'vault' && (
          <>
            <div className="grid grid-cols-2 gap-3">
              <FormField label={intl.formatMessage({ id: 'settings.system.vaultAddr' })}>
                <input type="text" value={vaultAddr} onChange={(e) => setVaultAddr(e.target.value)} placeholder="https://vault:8200" className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'settings.system.vaultMount' })}>
                <input type="text" value={vaultMount} onChange={(e) => setVaultMount(e.target.value)} placeholder="secret" className={inputClass} />
              </FormField>
            </div>
            <FormField label={intl.formatMessage({ id: 'settings.system.vaultToken' })} hint={intl.formatMessage({ id: 'settings.system.writeOnly' })}>
              <input type="password" value={vaultToken} onChange={(e) => setVaultToken(e.target.value)} placeholder="••••••••" className={inputClass} autoComplete="off" />
            </FormField>
          </>
        )}
      </Card>

      <div className="flex items-center justify-end gap-2">
        {saved && (
          <span className="text-xs text-emerald-600 dark:text-emerald-400">
            {intl.formatMessage({ id: 'settings.general.saved' })}
          </span>
        )}
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </div>
  );
}

function ContainerTab() {
  const intl = useIntl();

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <Container className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.container' })}
        </span>
      }
    >
      <div className="space-y-4">
        <SettingRow label="Engine" value="Docker" />
        <SettingRow label="Socket" value="/var/run/docker.sock" />
        <SettingRow label="Status" value="Detected" badge="emerald" />
      </div>
    </Card>
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
      .catch((e) => {
        console.warn("[api]", e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
    // 15s poll stays silent — transient errors would spam the user; the
    // initial load toast is enough to flag persistent problems.
    const interval = setInterval(() => {
      api.heartbeat
        .status()
        .then((r) => setHeartbeats(r?.heartbeats ?? []))
        .catch((e) => console.warn("[api]", e));
    }, 15_000);
    return () => clearInterval(interval);
  }, [intl]);

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <HeartPulse className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.heartbeat' })}
        </span>
      }
    >
      {heartbeats.length === 0 ? (
        <EmptyState
          icon={HeartPulse}
          title={intl.formatMessage({ id: 'common.noData' })}
        />
      ) : (
        <div className="space-y-3">
          {heartbeats.map((hb) => (
            <div
              key={hb.agent_id}
              className="flex items-center justify-between rounded-lg bg-stone-500/5 p-3 dark:bg-white/5"
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
                    }).catch((e) => {
                      console.warn("[api]", e);
                      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
                    });
                  }}
                  title={intl.formatMessage({ id: 'settings.heartbeat.toggle' })}
                  className={cn(
                    'inline-block h-2.5 w-2.5 rounded-full cursor-pointer ring-2 ring-offset-1 ring-transparent hover:ring-amber-400 transition-all',
                    hb.enabled ? 'bg-emerald-500' : 'bg-stone-300 dark:bg-stone-600'
                  )}
                />
                <button
                  onClick={() => api.heartbeat.trigger(hb.agent_id).catch((e) => {
                    console.warn("[api]", e);
                    toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
                  })}
                  className="rounded px-1.5 py-0.5 text-xs text-amber-600 hover:bg-amber-500/10 dark:text-amber-400"
                >
                  <Play className="h-3 w-3" />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}

type CronTaskItem = {
  id?: string;
  name?: string;
  agent_id: string;
  cron?: string;
  schedule?: string;
  task?: string;
  enabled: boolean;
  last_run_at?: string | null;
  last_status?: string | null;
};

/** Backend RPCs identify cron tasks by `id`; `name` is display-only. */
const cronTaskId = (t: CronTaskItem) => t.id ?? t.name ?? '';

function CronEditDialog({
  task,
  onClose,
  onSaved,
}: {
  task: CronTaskItem;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const intl = useIntl();
  const [name, setName] = useState(task.name ?? '');
  const [schedule, setSchedule] = useState(task.schedule ?? task.cron ?? '');
  const [agent, setAgent] = useState(task.agent_id);
  const [body, setBody] = useState(task.task ?? '');
  const [saving, setSaving] = useState(false);

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.cron.update(cronTaskId(task), {
        name: name.trim() || undefined,
        agent_id: agent.trim() || undefined,
        cron: schedule.trim() || undefined,
        task: body.trim() || undefined,
      });
      toast.success(intl.formatMessage({ id: 'common.saved' }));
      await onSaved();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'settings.cron.editTitle' })}>
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'settings.cron.name' })} htmlFor="cron-edit-name">
          <input id="cron-edit-name" type="text" value={name} onChange={(e) => setName(e.target.value)} className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'settings.cron.schedule' })} htmlFor="cron-edit-schedule" hint="m h dom mon dow">
          <input id="cron-edit-schedule" type="text" value={schedule} onChange={(e) => setSchedule(e.target.value)} className={cn(inputClass, 'font-mono')} placeholder="0 * * * *" />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'settings.cron.agent' })} htmlFor="cron-edit-agent">
          <input id="cron-edit-agent" type="text" value={agent} onChange={(e) => setAgent(e.target.value)} className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'settings.cron.task' })} htmlFor="cron-edit-task">
          <textarea id="cron-edit-task" rows={3} value={body} onChange={(e) => setBody(e.target.value)} className={inputClass} />
        </FormField>
        <div className="flex justify-end gap-2 pt-1">
          <button onClick={onClose} className={buttonSecondary}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button onClick={handleSave} disabled={saving || !schedule.trim()} className={buttonPrimary}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function CronTab() {
  const intl = useIntl();
  const [tasks, setTasks] = useState<ReadonlyArray<CronTaskItem>>([]);
  const [showAdd, setShowAdd] = useState(false);
  const [editing, setEditing] = useState<CronTaskItem | null>(null);
  const [newName, setNewName] = useState('');
  const [newSchedule, setNewSchedule] = useState('0 * * * *');
  const [newAgent, setNewAgent] = useState('');
  const [newTask, setNewTask] = useState('');
  const [adding, setAdding] = useState(false);

  const reportError = useCallback(
    (e: unknown) => {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    },
    [intl]
  );

  const fetchTasks = useCallback(async () => {
    try {
      const result = await api.cron.list();
      setTasks(result?.tasks ?? []);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => {
    fetchTasks();
  }, [fetchTasks]);

  const handleAdd = async () => {
    if (!newName.trim()) return;
    setAdding(true);
    try {
      await api.cron.add({
        name: newName.trim(),
        agent_id: newAgent.trim() || 'default',
        cron: newSchedule.trim(),
        task: newTask.trim() || undefined,
      });
      setShowAdd(false);
      setNewName('');
      setNewSchedule('0 * * * *');
      setNewAgent('');
      setNewTask('');
      await fetchTasks();
    } catch (e) {
      reportError(e);
    } finally {
      setAdding(false);
    }
  };

  const handlePause = async (id: string) => {
    try {
      await api.cron.pause(id);
      await fetchTasks();
    } catch (e) {
      reportError(e);
    }
  };

  const handleResume = async (id: string) => {
    try {
      await api.cron.resume(id);
      await fetchTasks();
    } catch (e) {
      reportError(e);
    }
  };

  const handleRemove = async (id: string) => {
    try {
      await api.cron.remove(id);
      await fetchTasks();
    } catch (e) {
      reportError(e);
    }
  };

  const inputStyle = controlClass;

  return (
    <Card
      padded={false}
      title={
        <span className="flex items-center gap-2">
          <Clock className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.cron' })}
        </span>
      }
      actions={
        <Button variant="primary" size="sm" icon={Plus} onClick={() => setShowAdd(!showAdd)}>
          {intl.formatMessage({ id: 'settings.cron.add' })}
        </Button>
      }
    >
      {/* Edit dialog */}
      {editing && (
        <CronEditDialog
          task={editing}
          onClose={() => setEditing(null)}
          onSaved={fetchTasks}
        />
      )}

      {/* Add task form */}
      {showAdd && (
        <div className="m-5 mb-0 rounded-xl bg-amber-500/8 p-4 ring-1 ring-inset ring-amber-500/20">
          <div className="grid gap-3 sm:grid-cols-3">
            <input type="text" placeholder={intl.formatMessage({ id: 'settings.cron.name' })} value={newName} onChange={(e) => setNewName(e.target.value)} className={inputStyle} />
            <input type="text" placeholder="0 * * * *" value={newSchedule} onChange={(e) => setNewSchedule(e.target.value)} className={cn(inputStyle, 'font-mono')} />
            <input type="text" placeholder={intl.formatMessage({ id: 'settings.cron.agent' })} value={newAgent} onChange={(e) => setNewAgent(e.target.value)} className={inputStyle} />
          </div>
          <textarea
            rows={2}
            placeholder={intl.formatMessage({ id: 'settings.cron.task' })}
            value={newTask}
            onChange={(e) => setNewTask(e.target.value)}
            className={cn(inputStyle, 'mt-3 h-auto w-full py-2')}
          />
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="secondary" size="sm" onClick={() => setShowAdd(false)}>
              {intl.formatMessage({ id: 'common.cancel' })}
            </Button>
            <Button variant="primary" size="sm" onClick={handleAdd} disabled={adding || !newName.trim()}>
              {adding ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
            </Button>
          </div>
        </div>
      )}

      {tasks.length === 0 ? (
        <EmptyState
          icon={Clock}
          title={intl.formatMessage({ id: 'common.noData' })}
        />
      ) : (
        <div className="overflow-x-auto px-5 pb-2">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-[var(--panel-border)]">
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
                const taskId = cronTaskId(task);
                const taskLabel = task.name ?? task.id ?? '';
                const taskCron = task.schedule ?? task.cron ?? '';
                return (
                  <tr
                    key={taskId}
                    className="border-b border-[var(--panel-border)] last:border-0"
                  >
                    <td className="py-2 font-mono text-xs text-stone-700 dark:text-stone-300">
                      {taskLabel}
                    </td>
                    <td className="py-2 text-stone-700 dark:text-stone-300">
                      {task.agent_id}
                    </td>
                    <td className="py-2">
                      <code className="rounded bg-stone-500/10 px-2 py-0.5 font-mono text-xs text-stone-600 dark:text-stone-400">
                        {taskCron}
                      </code>
                    </td>
                    <td className="py-2 text-center">
                      {task.enabled ? (
                        <Badge tone="success">{intl.formatMessage({ id: 'settings.cron.enabled' })}</Badge>
                      ) : (
                        <Badge tone="neutral">{intl.formatMessage({ id: 'settings.cron.disabled' })}</Badge>
                      )}
                    </td>
                    <td className="py-2 text-right">
                      <div className="flex justify-end gap-1">
                        <Button variant="ghost" size="sm" icon={Pencil} onClick={() => setEditing(task)}>
                          {intl.formatMessage({ id: 'common.edit' })}
                        </Button>
                        {task.enabled ? (
                          <button
                            onClick={() => handlePause(taskId)}
                            className="rounded px-2 py-1 text-xs text-amber-600 hover:bg-amber-500/10 dark:text-amber-400"
                          >
                            {intl.formatMessage({ id: 'settings.cron.pause' })}
                          </button>
                        ) : (
                          <button
                            onClick={() => handleResume(taskId)}
                            className="rounded px-2 py-1 text-xs text-emerald-600 hover:bg-emerald-500/10 dark:text-emerald-400"
                          >
                            {intl.formatMessage({ id: 'settings.cron.resume' })}
                          </button>
                        )}
                        <button
                          onClick={() => handleRemove(taskId)}
                          className="rounded px-2 py-1 text-xs text-rose-600 hover:bg-rose-500/10 dark:text-rose-400"
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
    </Card>
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
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex gap-2">
        <Button variant="primary" icon={Play} onClick={runDoctor} disabled={loading}>
          {intl.formatMessage({ id: 'settings.doctor.run' })}
        </Button>
        <Button variant="secondary" icon={Wrench} onClick={handleRepair} disabled={loading}>
          {intl.formatMessage({ id: 'settings.doctor.repair' })}
        </Button>
      </div>

      {doctorChecks.length === 0 ? (
        <Card padded={false}>
          <EmptyState
            icon={Stethoscope}
            title={intl.formatMessage({ id: 'settings.doctor.run' })}
          />
        </Card>
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
  const [autoUpdate, setAutoUpdate] = useState(false);
  const [edition, setEdition] = useState('community');
  const [updateInfo, setUpdateInfo] = useState<{
    available: boolean;
    current_version: string;
    latest_version: string;
    release_notes: string;
    published_at: string;
    download_url: string;
    install_method: string;
    brew_formula?: string;
    auto_update?: boolean;
  } | null>(null);

  // Load edition + auto_update state on mount
  useEffect(() => {
    api.system.version().then((info) => {
      setEdition(info.edition ?? 'community');
      setAutoUpdate(info.auto_update ?? false);
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

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
  const isPro = edition !== 'community';

  const handleAutoUpdateToggle = useCallback(async (enabled: boolean) => {
    try {
      await api.system.updateConfig({ auto_update: enabled });
      setAutoUpdate(enabled);
    } catch (e) {
      // state was never flipped, so no revert needed — just surface the failure
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  return (
    <div className="space-y-6">
      {/* Auto-update toggle — Pro only */}
      {isPro && (
        <Card>
          <div className="flex items-center justify-between">
            <div>
              <h3 className="text-sm font-medium text-stone-900 dark:text-stone-50">
                {intl.formatMessage({ id: 'settings.update.autoUpdate' })}
              </h3>
              <p className="mt-1 text-xs text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'settings.update.autoUpdate.desc' })}
              </p>
            </div>
            <label className="relative inline-flex cursor-pointer items-center">
              <input
                type="checkbox"
                checked={autoUpdate}
                onChange={(e) => handleAutoUpdateToggle(e.target.checked)}
                className="peer sr-only"
              />
              <div className="peer h-6 w-11 rounded-full bg-stone-200 after:absolute after:left-[2px] after:top-[2px] after:h-5 after:w-5 after:rounded-full after:bg-white after:transition-all peer-checked:bg-amber-500 peer-checked:after:translate-x-full dark:bg-stone-600" />
            </label>
          </div>
        </Card>
      )}

      <Card
        title={
          <span className="flex items-center gap-2">
            <ArrowUpCircle className="h-4 w-4 text-amber-500" />
            {intl.formatMessage({ id: 'settings.update' })}
          </span>
        }
        actions={
          <Button variant="primary" icon={RefreshCw} onClick={handleCheck} disabled={checking}>
            {checking
              ? intl.formatMessage({ id: 'settings.update.checking' })
              : intl.formatMessage({ id: 'settings.update.check' })}
          </Button>
        }
      >
        {/* Status display */}
        {!updateInfo && !error && (
          <EmptyState
            icon={Download}
            title={intl.formatMessage({ id: 'settings.update.check' })}
          />
        )}

        {error && (
          <div className="rounded-lg bg-rose-500/10 p-4 ring-1 ring-inset ring-rose-500/20">
            <div className="flex items-center gap-2">
              <XCircle className="h-5 w-5 text-rose-500" />
              <span className="text-sm text-rose-700 dark:text-rose-400">{error}</span>
            </div>
          </div>
        )}

        {installed && (
          <div className="rounded-lg bg-emerald-500/10 p-4 ring-1 ring-inset ring-emerald-500/20">
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
              <div className="rounded-lg bg-stone-500/5 p-4 dark:bg-white/5">
                <span className="text-xs text-stone-400">
                  {intl.formatMessage({ id: 'settings.update.current' })}
                </span>
                <p className="mt-1 text-lg font-semibold text-stone-900 dark:text-stone-50">
                  v{updateInfo.current_version}
                </p>
              </div>
              <div className={cn(
                'rounded-lg p-4 ring-1 ring-inset',
                updateInfo.available
                  ? 'bg-amber-500/10 ring-amber-500/20'
                  : 'bg-emerald-500/10 ring-emerald-500/20'
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
              <div className="flex items-center gap-2 rounded-lg bg-emerald-500/10 p-4 ring-1 ring-inset ring-emerald-500/20">
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
                  <div className="rounded-lg p-4 ring-1 ring-inset ring-[var(--panel-border)]">
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
                  <div className="rounded-lg bg-amber-500/10 p-4 ring-1 ring-inset ring-amber-500/20">
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
                  <div className="rounded-lg bg-amber-500/10 p-4 ring-1 ring-inset ring-amber-500/20">
                    <p className="text-sm text-amber-700 dark:text-amber-400">
                      {intl.formatMessage({ id: 'settings.update.noBinary' })}
                    </p>
                  </div>
                )}

                {/* Install button */}
                {!isHomebrew && !noBinary && (
                  <Button
                    variant="primary"
                    icon={Download}
                    onClick={handleInstall}
                    disabled={installing}
                    className="w-full py-3"
                  >
                    {installing
                      ? intl.formatMessage({ id: 'settings.update.installing' })
                      : intl.formatMessage({ id: 'settings.update.install' })}
                  </Button>
                )}
              </>
            )}
          </div>
        )}
      </Card>
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
  const badgeTone = { emerald: 'success', amber: 'warning', rose: 'danger' } as const;

  return (
    <div className="flex items-center justify-between border-b border-[var(--panel-border)] pb-3 last:border-0 last:pb-0">
      <span className="text-sm text-stone-600 dark:text-stone-400">
        {label}
      </span>
      {badge ? (
        <Badge tone={badgeTone[badge]}>{value}</Badge>
      ) : (
        <span className="text-sm font-medium text-stone-900 dark:text-stone-50">
          {value}
        </span>
      )}
    </div>
  );
}

// ── Privacy / Redaction Tab (RED) ──────────────────────────────

const REDACTION_SOURCE_KEYS: ReadonlyArray<keyof RedactionSources> = [
  'user_input',
  'tool_results',
  'system_prompt',
  'sub_agent',
  'cron_context',
];
const REDACTION_MODES: ReadonlyArray<RedactionSourceMode> = ['on', 'off', 'selective', 'inherit'];
const REDACTION_RESTORE: ReadonlyArray<RedactionRestoreArgs> = ['restore', 'passthrough', 'deny'];

function SkillSynthesisTab() {
  const intl = useIntl();
  const [config, setConfig] = useState<SkillSynthesisConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    try {
      setConfig(await api.skillSynthesis.get());
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    try {
      await api.skillSynthesis.update({
        auto_run: config.auto_run,
        dry_run: config.dry_run,
        interval_hours: config.interval_hours,
        lookback_days: config.lookback_days,
        target_agent: config.target_agent.trim(),
      });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  if (!config) {
    return (
      <Card>
        <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
      </Card>
    );
  }

  return (
    <Card
      bodyClassName="space-y-6"
      title={
        <span className="flex items-center gap-2">
          <Sparkles className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.skillSynthesis' })}
        </span>
      }
    >
      <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'skillSynthesis.desc' })}</p>

      {/* Master toggle: auto_run */}
      <label className="flex items-center justify-between gap-3 py-1.5">
        <span>
          <span className="block text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'skillSynthesis.autoRun' })}</span>
          <span className="block text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'skillSynthesis.autoRun.hint' })}</span>
        </span>
        <input
          type="checkbox"
          checked={config.auto_run}
          onChange={(e) => setConfig({ ...config, auto_run: e.target.checked })}
          className="h-4 w-4 shrink-0 accent-amber-500"
        />
      </label>

      {/* dry_run toggle */}
      <label className="flex items-center justify-between gap-3 py-1.5">
        <span>
          <span className="block text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'skillSynthesis.dryRun' })}</span>
          <span className="block text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'skillSynthesis.dryRun.hint' })}</span>
        </span>
        <input
          type="checkbox"
          checked={config.dry_run}
          onChange={(e) => setConfig({ ...config, dry_run: e.target.checked })}
          className="h-4 w-4 shrink-0 accent-amber-500"
        />
      </label>

      {/* Live-mode warning when writes are enabled */}
      {config.auto_run && !config.dry_run && (
        <div className="flex items-start gap-2 rounded-lg border border-amber-300/60 bg-amber-50/60 px-3 py-2 text-xs text-amber-700 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-400">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <div className="space-y-1.5">
            <p>{intl.formatMessage({ id: 'skillSynthesis.liveWarning' })}</p>
            <p>
              {intl.formatMessage({ id: 'skillSynthesis.apiKeyHelp' })}{' '}
              <a
                href="https://console.anthropic.com/settings/keys"
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-0.5 font-medium underline underline-offset-2 hover:text-amber-800 dark:hover:text-amber-300"
              >
                {intl.formatMessage({ id: 'skillSynthesis.apiKeyLink' })}
                <ExternalLink className="h-3 w-3" />
              </a>
            </p>
          </div>
        </div>
      )}

      {/* Scalars */}
      <div className="grid gap-4 sm:grid-cols-2">
        <FormField label={intl.formatMessage({ id: 'skillSynthesis.intervalHours' })} hint="1-168">
          <input
            type="number"
            min={1}
            max={168}
            value={config.interval_hours}
            onChange={(e) => setConfig({ ...config, interval_hours: Number(e.target.value) })}
            className={inputClass}
          />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'skillSynthesis.lookbackDays' })} hint="1-30">
          <input
            type="number"
            min={1}
            max={30}
            value={config.lookback_days}
            onChange={(e) => setConfig({ ...config, lookback_days: Number(e.target.value) })}
            className={inputClass}
          />
        </FormField>
      </div>

      <FormField label={intl.formatMessage({ id: 'skillSynthesis.targetAgent' })} hint={intl.formatMessage({ id: 'skillSynthesis.targetAgent.hint' })}>
        <input
          type="text"
          value={config.target_agent}
          onChange={(e) => setConfig({ ...config, target_agent: e.target.value })}
          placeholder={intl.formatMessage({ id: 'skillSynthesis.targetAgent.placeholder' })}
          className={inputClass}
        />
      </FormField>

      <div className="flex items-center justify-end gap-3 border-t border-[var(--panel-border)] pt-4">
        {saved && <span className="text-sm text-emerald-500">{intl.formatMessage({ id: 'settings.general.saved' })}</span>}
        <button onClick={handleSave} disabled={saving} className={buttonPrimary}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </button>
      </div>
    </Card>
  );
}

function RedactionTab() {
  const intl = useIntl();
  const [config, setConfig] = useState<RedactionConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [newTool, setNewTool] = useState('');
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    try {
      const res = await api.redaction.get();
      setConfig(res);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    try {
      const payload: RedactionUpdate = {
        enabled: config.enabled,
        vault_ttl_hours: config.vault_ttl_hours,
        purge_after_expire_days: config.purge_after_expire_days,
        profiles: config.profiles,
        sources: config.sources,
        tool_egress: config.tool_egress,
      };
      await api.redaction.update(payload);
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const addEgress = () => {
    const tool = newTool.trim();
    if (!tool || !config || config.tool_egress[tool]) {
      setNewTool('');
      return;
    }
    setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { restore_args: 'deny', audit_reveal: false } } });
    setNewTool('');
  };

  const removeEgress = (tool: string) => {
    if (!config) return;
    const next = { ...config.tool_egress };
    delete next[tool];
    setConfig({ ...config, tool_egress: next });
  };

  if (!config) {
    return (
      <Card>
        <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
      </Card>
    );
  }

  return (
    <Card
      bodyClassName="space-y-6"
      title={
        <span className="flex items-center gap-2">
          <EyeOff className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.redaction' })}
        </span>
      }
    >
      <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.desc' })}</p>

      {/* Master toggle + scalars */}
      <div className="space-y-4">
        <label className="flex items-center justify-between py-1.5">
          <span className="text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'redaction.enabled' })}</span>
          <input type="checkbox" checked={config.enabled} onChange={(e) => setConfig({ ...config, enabled: e.target.checked })} className="h-4 w-4 accent-amber-500" />
        </label>
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={intl.formatMessage({ id: 'redaction.vaultTtl' })} hint="1-8760">
            <input type="number" min={1} max={8760} value={config.vault_ttl_hours} onChange={(e) => setConfig({ ...config, vault_ttl_hours: Number(e.target.value) })} className={inputClass} />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'redaction.purgeAfter' })} hint="0-3650">
            <input type="number" min={0} max={3650} value={config.purge_after_expire_days} onChange={(e) => setConfig({ ...config, purge_after_expire_days: Number(e.target.value) })} className={inputClass} />
          </FormField>
        </div>
        <FormField label={intl.formatMessage({ id: 'redaction.profiles' })} hint={intl.formatMessage({ id: 'redaction.profiles.hint' })}>
          <ChipEditor values={config.profiles} onChange={(v) => setConfig({ ...config, profiles: v })} placeholder="pii" addLabel={intl.formatMessage({ id: 'common.add' })} />
        </FormField>
      </div>

      {/* Sources matrix */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-3 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'redaction.sources' })}</h4>
        <div className="space-y-3">
          {REDACTION_SOURCE_KEYS.map((key) => (
            <div key={key} className="flex items-center justify-between gap-3">
              <span className="text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: `redaction.source.${key}` })}</span>
              <select
                value={config.sources[key]}
                onChange={(e) => setConfig({ ...config, sources: { ...config.sources, [key]: e.target.value as RedactionSourceMode } })}
                className={cn(selectClass, 'w-40')}
              >
                {REDACTION_MODES.map((m) => (
                  <option key={m} value={m}>{intl.formatMessage({ id: `redaction.mode.${m}` })}</option>
                ))}
              </select>
            </div>
          ))}
        </div>
      </div>

      {/* Tool egress rules */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-3 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'redaction.toolEgress' })}</h4>
        <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.toolEgress.hint' })}</p>
        <div className="space-y-2">
          {(Object.entries(config.tool_egress) as Array<[string, RedactionEgressRule]>).map(([tool, rule]) => (
            <div key={tool} className="flex flex-wrap items-center gap-2 rounded-lg bg-stone-500/5 p-2.5 dark:bg-white/5">
              <code className="rounded bg-stone-500/10 px-2 py-0.5 font-mono text-xs text-stone-700 dark:text-stone-300">{tool}</code>
              <select
                value={rule.restore_args}
                onChange={(e) => setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { ...rule, restore_args: e.target.value as RedactionRestoreArgs } } })}
                className={cn(selectClass, 'w-36')}
              >
                {REDACTION_RESTORE.map((r) => (
                  <option key={r} value={r}>{intl.formatMessage({ id: `redaction.restore.${r}` })}</option>
                ))}
              </select>
              <label className="flex items-center gap-1.5 text-xs text-stone-600 dark:text-stone-400">
                <input type="checkbox" checked={rule.audit_reveal} onChange={(e) => setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { ...rule, audit_reveal: e.target.checked } } })} className="accent-amber-500" />
                {intl.formatMessage({ id: 'redaction.auditReveal' })}
              </label>
              <button onClick={() => removeEgress(tool)} className="ml-auto rounded p-1 text-rose-500 hover:bg-rose-500/10">
                <Trash2 className="h-3.5 w-3.5" />
              </button>
            </div>
          ))}
          {Object.keys(config.tool_egress).length === 0 && (
            <p className="text-xs text-stone-400">{intl.formatMessage({ id: 'common.noData' })}</p>
          )}
        </div>
        <div className="mt-3 flex gap-2">
          <input type="text" value={newTool} onChange={(e) => setNewTool(e.target.value)} placeholder={intl.formatMessage({ id: 'redaction.toolEgress.toolName' })} className={cn(inputClass, 'flex-1')} />
          <Button variant="secondary" icon={Plus} onClick={addEgress}>
            {intl.formatMessage({ id: 'redaction.toolEgress.add' })}
          </Button>
        </div>
      </div>

      <div className="flex items-center justify-end gap-2 pt-2">
        {saved && <span className="text-xs text-emerald-600 dark:text-emerald-400">{intl.formatMessage({ id: 'settings.general.saved' })}</span>}
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </Card>
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
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  // Load persisted [voice] settings from inference.toml on mount.
  useEffect(() => {
    api.system.config().then((res) => {
      if (res?.voice) {
        setConfig((prev) => ({ ...prev, ...res.voice }));
      }
    }).catch((e) => {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.system.updateConfig({ voice: config });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card title={intl.formatMessage({ id: 'voice.title' })} bodyClassName="space-y-6">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label={intl.formatMessage({ id: 'voice.asrProvider' })}>
          <select
            value={config.asr_provider}
            onChange={(e) => setConfig({ ...config, asr_provider: e.target.value })}
            className={controlClass}
          >
            <option value="auto">Auto</option>
            <option value="whisper-api">{intl.formatMessage({ id: 'voice.provider.whisperApi' })}</option>
            <option value="whisper-local">Whisper Local</option>
            <option value="sensevoice">{intl.formatMessage({ id: 'voice.provider.sensevoice' })}</option>
          </select>
        </Field>

        <Field label={intl.formatMessage({ id: 'voice.ttsProvider' })}>
          <select
            value={config.tts_provider}
            onChange={(e) => setConfig({ ...config, tts_provider: e.target.value })}
            className={controlClass}
          >
            <option value="auto">Auto</option>
            <option value="edge-tts">{intl.formatMessage({ id: 'voice.provider.edgeTts' })}</option>
            <option value="minimax">{intl.formatMessage({ id: 'voice.provider.minimax' })}</option>
            <option value="openai-tts">{intl.formatMessage({ id: 'voice.provider.openaiTts' })}</option>
            <option value="piper">{intl.formatMessage({ id: 'voice.provider.piper' })}</option>
          </select>
        </Field>

        <Field label={intl.formatMessage({ id: 'voice.language' })}>
          <select
            value={config.asr_language}
            onChange={(e) => setConfig({ ...config, asr_language: e.target.value })}
            className={controlClass}
          >
            <option value="zh">中文 (zh)</option>
            <option value="en">English (en)</option>
            <option value="ja">日本語 (ja)</option>
            <option value="ko">한국어 (ko)</option>
          </select>
        </Field>

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
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saved
            ? intl.formatMessage({ id: 'settings.general.saved' })
            : saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </Card>
  );
}

// ── Proactive Settings Tab ─────────────────────────────────────

function ProactiveTab() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState('');
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
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  useEffect(() => {
    if (agents.length > 0 && !selectedAgent) setSelectedAgent(agents[0].name);
  }, [agents, selectedAgent]);

  // Proactive settings live in each agent's agent.toml [proactive] section.
  useEffect(() => {
    if (!selectedAgent) return;
    api.agents.inspect(selectedAgent).then((detail) => {
      // Always reset — switching to an agent without a [proactive] section
      // must not carry over the previous agent's values.
      setConfig({
        enabled: detail?.proactive?.enabled ?? false,
        check_interval: detail?.proactive?.check_interval ?? '*/30 * * * *',
        quiet_hours_start: detail?.proactive?.quiet_hours_start ?? 23,
        quiet_hours_end: detail?.proactive?.quiet_hours_end ?? 8,
        max_messages_per_hour: detail?.proactive?.max_messages_per_hour ?? 3,
        notify_channel: detail?.proactive?.notify_channel ?? '',
        notify_chat_id: detail?.proactive?.notify_chat_id ?? '',
      });
    }).catch((e) => {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [selectedAgent, intl]);

  const handleSave = async () => {
    if (!selectedAgent) return;
    setSaving(true);
    try {
      await api.agents.update(selectedAgent, { proactive: config });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card
      bodyClassName="space-y-6"
      title={intl.formatMessage({ id: 'proactive.title' })}
      actions={
        <select
          value={selectedAgent}
          onChange={(e) => setSelectedAgent(e.target.value)}
          className={cn(controlClass, 'h-8 w-auto min-w-[8rem] text-xs')}
        >
          {agents.length === 0 && <option value="">{intl.formatMessage({ id: 'common.noData' })}</option>}
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
      }
    >
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

        <Field
          label={intl.formatMessage({ id: 'proactive.checkInterval' })}
          help={intl.formatMessage({ id: 'proactive.checkInterval.hint' })}
        >
          <input
            type="text"
            value={config.check_interval}
            onChange={(e) => setConfig({ ...config, check_interval: e.target.value })}
            placeholder="*/30 * * * *"
            className={controlClass}
          />
        </Field>

        <Field label={intl.formatMessage({ id: 'proactive.quietHours' })}>
          <div className="flex items-center gap-2">
            <input type="number" min={0} max={23} value={config.quiet_hours_start}
              onChange={(e) => setConfig({ ...config, quiet_hours_start: +e.target.value })}
              className={cn(controlClass, 'w-16 px-2 text-center')} />
            <span className="text-stone-400">—</span>
            <input type="number" min={0} max={23} value={config.quiet_hours_end}
              onChange={(e) => setConfig({ ...config, quiet_hours_end: +e.target.value })}
              className={cn(controlClass, 'w-16 px-2 text-center')} />
          </div>
        </Field>

        <Field label={intl.formatMessage({ id: 'proactive.maxMessagesPerHour' })}>
          <input type="number" min={1} max={60} value={config.max_messages_per_hour}
            onChange={(e) => setConfig({ ...config, max_messages_per_hour: +e.target.value })}
            className={cn(controlClass, 'w-24')} />
        </Field>

        <Field label={intl.formatMessage({ id: 'proactive.notifyChannel' })}>
          <select value={config.notify_channel}
            onChange={(e) => setConfig({ ...config, notify_channel: e.target.value })}
            className={controlClass}>
            <option value="">{intl.formatMessage({ id: 'proactive.selectChannel' })}</option>
            <option value="telegram">Telegram</option>
            <option value="line">LINE</option>
            <option value="discord">Discord</option>
          </select>
        </Field>

        <Field label={intl.formatMessage({ id: 'proactive.chatId' })}>
          <input type="text" value={config.notify_chat_id}
            onChange={(e) => setConfig({ ...config, notify_chat_id: e.target.value })}
            placeholder="e.g., 123456789"
            className={controlClass} />
        </Field>
      </div>

      <div className="flex justify-end pt-2">
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saved
            ? intl.formatMessage({ id: 'settings.general.saved' })
            : saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </Card>
  );
}

// ── Autopilot Tab ───────────────────────────────────────────

function AutopilotTab() {
  const intl = useIntl();
  const [rules, setRules] = useState<AutopilotRule[]>([]);
  const [loading, setLoading] = useState(true);
  const [showCreate, setShowCreate] = useState(false);
  const [historyRuleId, setHistoryRuleId] = useState<string | null>(null);
  const [historyEntries, setHistoryEntries] = useState<AutopilotHistoryEntry[]>([]);
  const [removeTarget, setRemoveTarget] = useState<{ id: string; name: string } | null>(null);

  const fetchRules = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.autopilot.list();
      setRules(result?.rules ?? []);
    } catch (e) {
      setRules([]);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => { fetchRules(); }, [fetchRules]);

  const handleToggle = useCallback(async (ruleId: string, enabled: boolean) => {
    setRules((prev) => prev.map((r) => (r.id === ruleId ? { ...r, enabled } : r)));
    try {
      await api.autopilot.update(ruleId, { enabled });
    } catch (e) {
      setRules((prev) => prev.map((r) => (r.id === ruleId ? { ...r, enabled: !enabled } : r)));
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  const handleRemove = useCallback(async () => {
    if (!removeTarget) return;
    try {
      await api.autopilot.remove(removeTarget.id);
      setRules((prev) => prev.filter((r) => r.id !== removeTarget.id));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
    setRemoveTarget(null);
  }, [removeTarget, intl]);

  const handleViewHistory = useCallback(async (ruleId: string) => {
    setHistoryRuleId(ruleId);
    try {
      const result = await api.autopilot.history(ruleId);
      setHistoryEntries(result?.entries ?? []);
    } catch (e) {
      setHistoryEntries([]);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  return (
    <div className="space-y-6">
      <Section
        title={intl.formatMessage({ id: 'autopilot.title' })}
        description={intl.formatMessage({ id: 'autopilot.subtitle' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setShowCreate(true)}>
            {intl.formatMessage({ id: 'autopilot.create' })}
          </Button>
        }
      >
        {loading ? (
          <div className="py-12 text-center text-stone-400">
            {intl.formatMessage({ id: 'common.loading' })}
          </div>
        ) : rules.length === 0 ? (
          <Card padded={false}>
            <EmptyState
              icon={Workflow}
              title={intl.formatMessage({ id: 'autopilot.empty' })}
            />
          </Card>
        ) : (
        <div className="space-y-3">
          {rules.map((rule) => (
            <Card key={rule.id} className="p-5" padded={false}>
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <button
                    onClick={() => handleToggle(rule.id, !rule.enabled)}
                    className={cn(
                      'relative h-6 w-11 rounded-full transition-colors',
                      rule.enabled ? 'bg-emerald-500' : 'bg-stone-300 dark:bg-stone-600',
                    )}
                  >
                    <span
                      className={cn(
                        'absolute top-0.5 h-5 w-5 rounded-full bg-white shadow-sm transition-transform',
                        rule.enabled ? 'left-[22px]' : 'left-0.5',
                      )}
                    />
                  </button>
                  <div>
                    <h4 className="font-medium text-stone-900 dark:text-stone-50">{rule.name}</h4>
                    <div className="mt-1 flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                      <Badge tone="info">
                        {intl.formatMessage({ id: `autopilot.trigger.${rule.trigger_event}` })}
                      </Badge>
                      <span>→</span>
                      <Badge tone="accent">
                        {intl.formatMessage({ id: `autopilot.action.${rule.action.type}` })}
                      </Badge>
                      <span className="text-stone-400">({rule.action.agent_id})</span>
                    </div>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => handleViewHistory(rule.id)}
                    className="rounded-lg p-1.5 text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
                    title={intl.formatMessage({ id: 'autopilot.history' })}
                  >
                    <Clock className="h-4 w-4" />
                  </button>
                  <button
                    onClick={() => setRemoveTarget({ id: rule.id, name: rule.name })}
                    className="rounded-lg p-1.5 text-stone-400 transition-colors hover:bg-rose-500/10 hover:text-rose-500"
                  >
                    <XCircle className="h-4 w-4" />
                  </button>
                </div>
              </div>

              <div className="mt-3 flex items-center gap-4 text-xs text-stone-400 dark:text-stone-500">
                <span>{intl.formatMessage({ id: 'autopilot.triggerCount' }, { count: rule.trigger_count })}</span>
                {rule.last_triggered_at && (
                  <span>
                    {intl.formatMessage({ id: 'autopilot.lastTriggered' })}: {new Date(rule.last_triggered_at).toLocaleString('zh-TW')}
                  </span>
                )}
              </div>
            </Card>
          ))}
        </div>
        )}
      </Section>

      {/* Create Rule Dialog */}
      {showCreate && (
        <AutopilotCreateDialog
          onClose={() => setShowCreate(false)}
          onCreated={() => { setShowCreate(false); fetchRules(); }}
        />
      )}

      {/* History Dialog */}
      {historyRuleId && (
        <AutopilotHistoryDialog
          entries={historyEntries}
          onClose={() => setHistoryRuleId(null)}
        />
      )}

      {/* Remove Confirmation */}
      {removeTarget && (
        <Dialog open onClose={() => setRemoveTarget(null)} title={intl.formatMessage({ id: 'autopilot.remove' })}>
          <div className="space-y-4">
            <p className="text-sm text-stone-600 dark:text-stone-400">
              {intl.formatMessage({ id: 'autopilot.remove.confirm' }, { name: removeTarget.name })}
            </p>
            <div className="flex justify-end gap-3">
              <button
                onClick={() => setRemoveTarget(null)}
                className="rounded-lg border border-stone-300 px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-800"
              >
                {intl.formatMessage({ id: 'common.cancel' })}
              </button>
              <button
                onClick={handleRemove}
                className="rounded-lg bg-rose-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-rose-600"
              >
                {intl.formatMessage({ id: 'autopilot.remove' })}
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </div>
  );
}

function AutopilotCreateDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [name, setName] = useState('');
  const [triggerEvent, setTriggerEvent] = useState<string>('task_created');
  const [actionType, setActionType] = useState<string>('delegate');
  const [actionAgent, setActionAgent] = useState('');
  const [promptTemplate, setPromptTemplate] = useState('');
  const [skillName, setSkillName] = useState('');
  const [fromStatus, setFromStatus] = useState('');
  const [toStatus, setToStatus] = useState('');
  const [idleMinutes, setIdleMinutes] = useState('30');
  const [cronExpr, setCronExpr] = useState('');
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  useEffect(() => { if (agents.length > 0 && !actionAgent) setActionAgent(agents[0].name); }, [agents, actionAgent]);

  const triggerEvents = ['task_created', 'task_status_changed', 'channel_message', 'agent_idle', 'schedule'] as const;
  const actionTypes = ['delegate', 'notify', 'run_skill'] as const;
  const statuses = ['todo', 'in_progress', 'done', 'blocked'] as const;

  const handleSubmit = useCallback(async () => {
    if (!name.trim() || !actionAgent) return;
    setSubmitting(true);
    try {
      const conditions: Record<string, unknown> = {};
      if (triggerEvent === 'task_status_changed') {
        if (fromStatus) conditions.from_status = fromStatus;
        if (toStatus) conditions.to_status = toStatus;
      }
      if (triggerEvent === 'agent_idle' && idleMinutes) {
        conditions.idle_minutes = parseInt(idleMinutes, 10);
      }
      if (triggerEvent === 'schedule' && cronExpr) {
        conditions.cron = cronExpr;
      }

      await api.autopilot.create({
        name: name.trim(),
        trigger_event: triggerEvent as typeof triggerEvents[number],
        conditions,
        action: {
          type: actionType as typeof actionTypes[number],
          agent_id: actionAgent,
          ...(actionType === 'delegate' && promptTemplate ? { prompt_template: promptTemplate } : {}),
          ...(actionType === 'run_skill' && skillName ? { skill_name: skillName } : {}),
        },
      });
      onCreated();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSubmitting(false);
    }
  }, [name, triggerEvent, actionType, actionAgent, promptTemplate, skillName, fromStatus, toStatus, idleMinutes, cronExpr, onCreated]);

  const inputCls = 'w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none focus:ring-2 focus:ring-amber-500/20 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50';
  const selectCls = inputCls;

  return (
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'autopilot.create' })} className="max-w-lg">
      <div className="space-y-4">
        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'autopilot.field.name' })}
          </label>
          <input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'autopilot.field.triggerEvent' })}
          </label>
          <select className={selectCls} value={triggerEvent} onChange={(e) => setTriggerEvent(e.target.value)}>
            {triggerEvents.map((t) => (
              <option key={t} value={t}>{intl.formatMessage({ id: `autopilot.trigger.${t}` })}</option>
            ))}
          </select>
        </div>

        {/* Conditional fields based on trigger type */}
        {triggerEvent === 'task_status_changed' && (
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
                {intl.formatMessage({ id: 'autopilot.field.fromStatus' })}
              </label>
              <select className={selectCls} value={fromStatus} onChange={(e) => setFromStatus(e.target.value)}>
                <option value="">{intl.formatMessage({ id: 'tasks.filter.all' })}</option>
                {statuses.map((s) => (
                  <option key={s} value={s}>{intl.formatMessage({ id: `tasks.status.${s}` })}</option>
                ))}
              </select>
            </div>
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
                {intl.formatMessage({ id: 'autopilot.field.toStatus' })}
              </label>
              <select className={selectCls} value={toStatus} onChange={(e) => setToStatus(e.target.value)}>
                <option value="">{intl.formatMessage({ id: 'tasks.filter.all' })}</option>
                {statuses.map((s) => (
                  <option key={s} value={s}>{intl.formatMessage({ id: `tasks.status.${s}` })}</option>
                ))}
              </select>
            </div>
          </div>
        )}

        {triggerEvent === 'agent_idle' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.idleMinutes' })}
            </label>
            <input className={inputCls} type="number" min="1" value={idleMinutes} onChange={(e) => setIdleMinutes(e.target.value)} />
          </div>
        )}

        {triggerEvent === 'schedule' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.cron' })}
            </label>
            <input className={inputCls} value={cronExpr} onChange={(e) => setCronExpr(e.target.value)} placeholder="0 9 * * 1-5" />
          </div>
        )}

        <div className="grid grid-cols-2 gap-3">
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.action' })}
            </label>
            <select className={selectCls} value={actionType} onChange={(e) => setActionType(e.target.value)}>
              {actionTypes.map((a) => (
                <option key={a} value={a}>{intl.formatMessage({ id: `autopilot.action.${a}` })}</option>
              ))}
            </select>
          </div>
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.actionAgent' })}
            </label>
            <select className={selectCls} value={actionAgent} onChange={(e) => setActionAgent(e.target.value)}>
              {agents.map((a) => (
                <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
              ))}
            </select>
          </div>
        </div>

        {actionType === 'delegate' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.promptTemplate' })}
            </label>
            <textarea
              className={cn(inputCls, 'min-h-[80px] resize-y')}
              value={promptTemplate}
              onChange={(e) => setPromptTemplate(e.target.value)}
              placeholder="Handle the newly created task: {{task.title}}"
            />
          </div>
        )}

        {actionType === 'run_skill' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.skillName' })}
            </label>
            <input className={inputCls} value={skillName} onChange={(e) => setSkillName(e.target.value)} />
          </div>
        )}

        <div className="flex justify-end gap-3 pt-2">
          <button
            onClick={onClose}
            className="rounded-lg border border-stone-300 px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-800"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !name.trim() || !actionAgent}
            className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            {intl.formatMessage({ id: 'autopilot.create' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function AutopilotHistoryDialog({
  entries,
  onClose,
}: {
  entries: ReadonlyArray<AutopilotHistoryEntry>;
  onClose: () => void;
}) {
  const intl = useIntl();
  return (
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'autopilot.history' })}>
      <div className="max-h-[400px] space-y-2 overflow-y-auto">
        {entries.length === 0 ? (
          <p className="py-8 text-center text-sm text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'autopilot.history.empty' })}
          </p>
        ) : (
          entries.map((entry) => (
            <div
              key={entry.id}
              className="flex items-center justify-between rounded-lg border border-stone-200 px-4 py-3 dark:border-stone-700"
            >
              <div>
                <span className="text-sm text-stone-700 dark:text-stone-300">
                  {new Date(entry.triggered_at).toLocaleString('zh-TW')}
                </span>
                {entry.details && (
                  <p className="mt-0.5 text-xs text-stone-400 dark:text-stone-500">{entry.details}</p>
                )}
              </div>
              <span
                className={cn(
                  'rounded-full px-2 py-0.5 text-xs font-medium',
                  entry.result === 'success'
                    ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
                    : 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
                )}
              >
                {intl.formatMessage({ id: `autopilot.history.${entry.result}` })}
              </span>
            </div>
          ))
        )}
      </div>
    </Dialog>
  );
}

// ── Browser Automation Tab ─────────────────────────────────────

function BrowserTab() {
  return (
    <div className="space-y-6">
      <ToolApprovalPanel />
      <SessionReplayPanel />
      <BrowserAuditPanel />
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
