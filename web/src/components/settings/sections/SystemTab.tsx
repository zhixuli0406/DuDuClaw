import { useCallback, useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { Plus, X } from 'lucide-react';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import { isImeComposing } from '@/lib/keyboard';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  ErrorState,
  Input,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
} from '@/components/mds';
import { AdvancedSection, DangerZone, type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch } from '@/pages/agent-form/form-rows';
import { useAuthStore } from '@/stores/auth-store';
import { useRestartRequiredStore } from '@/stores/restart-required-store';
import {
  changedPayload,
  parseTomlSubset,
  tomlBool,
  tomlNum,
  tomlSecretSet,
  tomlStr,
  tomlStrList,
  type FlatValues,
  type TomlTables,
} from '@/lib/config-toml';
import {
  FilesRootsSection,
  SANDBOX_DEFAULTS,
  SandboxSection,
  SecretBackendFields,
  TrustAndObservabilitySection,
  sandboxSizesInvalid,
} from './SystemTabExtras';

// Secret-manager backends the gateway's system.update_config accepts.
const SM_BACKENDS = ['local', 'vault', 'env', 'onepassword', 'infisical'];

// [gateway] bind presets shown in the dropdown; anything else is "custom".
type BindMode = 'loopback' | 'lan' | 'custom';
const bindToMode = (v: string): BindMode =>
  v === '0.0.0.0' ? 'lan' : v === '' || v === '127.0.0.1' ? 'loopback' : 'custom';

/**
 * Config keys the gateway names in its rejection messages → the i18n id of the
 * field label the user actually sees. This tab submits 20+ fields in ONE
 * payload, so "save failed" alone leaves the user hunting; when the server says
 * which key it choked on we point at that row's label. Purely additive: an
 * unrecognised message just shows no field hint (never a guessed one).
 */
const FIELD_LABEL_IDS: ReadonlyArray<readonly [RegExp, string]> = [
  [/\bbind\b/i, 'settings.system.bind'],
  [/\bport\b/i, 'settings.system.port'],
  [/\bauth_token\b/i, 'settings.system.authToken'],
  [/\bhealth_check_interval_seconds\b/i, 'settings.system.healthInterval'],
  [/\bcooldown_after_rate_limit_seconds\b/i, 'settings.system.cooldown'],
  [/\bdefault_agent\b/i, 'settings.system.defaultAgent'],
  [/\binference_mode\b/i, 'settings.system.inferenceMode'],
  [/\blog_format\b/i, 'settings.system.logFormat'],
  [/\ballowed_origins?\b/i, 'settings.system.remoteAccess'],
  [/\bvault_addr\b/i, 'settings.system.vaultAddr'],
  [/\bvault_mount\b/i, 'settings.system.vaultMount'],
  [/\bvault_token\b/i, 'settings.system.vaultToken'],
  [/\bsecret_manager\b|\bbackend\b/i, 'settings.system.smBackend'],
  [/\bdaily_digest(_at)?\b/i, 'settings.system.dailyDigest'],
  [/\bnovelty_gate\b/i, 'settings.system.noveltyGate'],
  [/\bmdns_advertise\b/i, 'settings.system.mdns'],
  [/\bname\b/i, 'settings.system.name'],
];

/** Field label ids named by a rejection message; empty when none match. */
function fieldsInError(err: unknown): string[] {
  const text =
    err instanceof Error ? err.message : typeof err === 'string' ? err : '';
  if (!text) return [];
  return FIELD_LABEL_IDS.filter(([re]) => re.test(text)).map(([, id]) => id);
}

// Client-side fail-closed IP check (the gateway re-validates authoritatively).
// Accepts IPv4 dotted-quad and loose IPv6; rejects hostnames / injection.
const isValidIp = (raw: string): boolean => {
  const s = raw.trim();
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(s)) return s.split('.').every((n) => Number(n) <= 255);
  return s.includes(':') && /^[0-9a-fA-F:]+$/.test(s);
};

/** v1.68 (W2) keys this tab reads from config.toml beyond the original set.
 *  Flat `system.update_config` param paths (= TOML paths). */
function readV168(tables: TomlTables): FlatValues {
  const sb = 'container.sandbox';
  return {
    'container.sandbox.image': tomlStr(tables, sb, 'image', ''),
    'container.sandbox.when_unavailable': tomlStr(tables, sb, 'when_unavailable', 'fail'),
    'container.sandbox.script_when_unavailable': tomlStr(tables, sb, 'script_when_unavailable', 'fail'),
    'container.sandbox.memory_bytes': tomlNum(tables, sb, 'memory_bytes', SANDBOX_DEFAULTS.memory_bytes),
    'container.sandbox.pids': tomlNum(tables, sb, 'pids', SANDBOX_DEFAULTS.pids),
    'container.sandbox.cpu_millis': tomlNum(tables, sb, 'cpu_millis', SANDBOX_DEFAULTS.cpu_millis),
    'container.sandbox.tmp_bytes': tomlNum(tables, sb, 'tmp_bytes', SANDBOX_DEFAULTS.tmp_bytes),
    'container.sandbox.workspace_bytes': tomlNum(tables, sb, 'workspace_bytes', SANDBOX_DEFAULTS.workspace_bytes),
    'container.sandbox.max_turns': tomlNum(tables, sb, 'max_turns', SANDBOX_DEFAULTS.max_turns),
    'computer_use.image': tomlStr(tables, 'computer_use', 'image', ''),
    'memory.supersession_trust_guard': tomlBool(tables, 'memory', 'supersession_trust_guard', true),
    'files.allowed_roots': tomlStrList(tables, 'files', 'allowed_roots'),
    'telemetry.otlp_endpoint': tomlStr(tables, 'telemetry', 'otlp_endpoint', ''),
    'acp.trusted': tomlBool(tables, 'acp', 'trusted', false),
    'integrations.github': tomlBool(tables, 'integrations', 'github', false),
    'secret_manager.onepassword_host': tomlStr(tables, 'secret_manager', 'onepassword_host', ''),
    'secret_manager.onepassword_vault': tomlStr(tables, 'secret_manager', 'onepassword_vault', ''),
    'secret_manager.infisical_addr': tomlStr(tables, 'secret_manager', 'infisical_addr', ''),
    'secret_manager.infisical_project_id': tomlStr(tables, 'secret_manager', 'infisical_project_id', ''),
    'secret_manager.infisical_environment': tomlStr(tables, 'secret_manager', 'infisical_environment', ''),
  };
}

const SANDBOX_NUMERIC_KEYS = [
  'container.sandbox.memory_bytes',
  'container.sandbox.pids',
  'container.sandbox.cpu_millis',
  'container.sandbox.tmp_bytes',
  'container.sandbox.workspace_bytes',
  'container.sandbox.max_turns',
] as const;

// ── G — System tab (gateway / rotation / general / logging / secret_manager) ──

export function SystemTab() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  // [server] — display name + LAN advertising (restart required).
  const [name, setName] = useState('');
  const [mdnsAdvertise, setMdnsAdvertise] = useState(false);
  // [gateway] — bind/port require restart; auth_token is write-only.
  const [bind, setBind] = useState('');
  const [bindMode, setBindMode] = useState<BindMode>('loopback');
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
  const [smBackend, setSmBackend] = useState('local');
  const [vaultAddr, setVaultAddr] = useState('');
  const [vaultMount, setVaultMount] = useState('');
  const [vaultToken, setVaultToken] = useState('');
  // [gateway] allowed_origins — remote-access allowlist (chips + draft input).
  const [origins, setOrigins] = useState<string[]>([]);
  const [originDraft, setOriginDraft] = useState('');
  // [memory] novelty_gate — B1 write-time memory dedup gate (default: on).
  const [noveltyGate, setNoveltyGate] = useState(true);
  // S20: `[miniapp] enabled` — the Telegram Mini App approval screen. Default
  // off; before 2026-09 it had no dashboard surface at all.
  const [miniappEnabled, setMiniappEnabled] = useState(false);
  // [notify] daily_digest / daily_digest_at — W2-8 daily-digest toggle
  // (default: off, matching `notify_digest::DigestConfig::default()`).
  const [dailyDigest, setDailyDigest] = useState(false);
  const [dailyDigestAt, setDailyDigestAt] = useState('09:00');
  // v1.68 (W2): the newer keys live in one flat map (see readV168).
  const [extra, setExtra] = useState<FlatValues>({});
  const setExtraKey = (key: string, value: unknown) => setExtra((prev) => ({ ...prev, [key]: value }));
  // Write-only secret-backend tokens + whether one is stored.
  const [smTokens, setSmTokens] = useState({ onepassword_token: '', infisical_token: '' });
  const [smTokenSet, setSmTokenSet] = useState({ onepassword_token: false, infisical_token: false });
  const [otelCompiled, setOtelCompiled] = useState(false);
  const isAdmin = useAuthStore((st) => st.user?.role === 'admin');
  // Loaded values, flattened — a save sends only what differs (plan principle 3).
  const initialRef = useRef<FlatValues>({});

  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // P05: 20+ fields render from compiled-in defaults when the read fails, so a
  // silent failure here shows a plausible but fictional gateway configuration.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);

  // `otel_compiled` decides whether the OTLP field can do anything at all.
  useEffect(() => {
    // Wrapped so a partial test mock (no `status`) degrades to "not compiled".
    Promise.resolve()
      .then(() => api.system.status())
      .then((st) => setOtelCompiled(st?.otel_compiled === true))
      .catch(() => setOtelCompiled(false));
  }, []);

  // Load non-secret current values from the TOML config string. Secrets
  // (auth_token / vault_token) are write-only — left blank, only sent if typed.
  const load = useCallback(() => {
    setLoadError(null);
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw !== 'string') return;
      const m = (re: RegExp) => raw.match(re)?.[1];
      // Scope name/mdns to their own sections so a `name`/`mdns_advertise`
      // elsewhere in config.toml can't be misread.
      const section = (h: string) =>
        raw.match(new RegExp(`\\[${h}\\]([\\s\\S]*?)(?:\\n\\s*\\[|$)`))?.[1] ?? '';
      const general = section('general');
      const server = section('server');
      setName(general.match(/\bname\s*=\s*"([^"]*)"/)?.[1] ?? '');
      setMdnsAdvertise(/\bmdns_advertise\s*=\s*true\b/.test(server));
      const bindVal = m(/\bbind\s*=\s*"([^"]*)"/) ?? '';
      setBind(bindVal);
      setBindMode(bindToMode(bindVal));
      setPort(m(/\bport\s*=\s*(\d+)/) ?? '');
      setHealthInterval(m(/health_check_interval_seconds\s*=\s*(\d+)/) ?? '');
      setCooldown(m(/cooldown_after_rate_limit_seconds\s*=\s*(\d+)/) ?? '');
      setDefaultAgent(m(/default_agent\s*=\s*"([^"]*)"/) ?? '');
      setInferenceMode(m(/inference_mode\s*=\s*"(\w+)"/) ?? 'claude');
      // `plain` is the contract spelling of the same output as `pretty`.
      setLogFormat(tomlStr(parseTomlSubset(raw), 'logging', 'format', 'pretty') === 'json' ? 'json' : 'pretty');
      // Gateway accepts local/vault/env/onepassword/infisical (serde default:
      // "local"). Map absent or legacy values (config/keychain) to "local" so
      // a save never carries a value the gateway rejects.
      const smRaw = m(/\bbackend\s*=\s*"(\w+)"/) ?? 'local';
      setSmBackend(SM_BACKENDS.includes(smRaw) ? smRaw : 'local');
      setVaultAddr(m(/vault_addr\s*=\s*"([^"]*)"/) ?? '');
      setVaultMount(m(/vault_mount\s*=\s*"([^"]*)"/) ?? '');
      // allowed_origins comes back as a structured array (not parsed from TOML).
      const ao = (res as Record<string, unknown>)?.allowed_origins;
      setOrigins(Array.isArray(ao) ? (ao.filter((v) => typeof v === 'string') as string[]) : []);
      // novelty_gate_enabled comes back structured too (not parsed from TOML).
      setNoveltyGate(res.novelty_gate_enabled ?? true);
      // miniapp_enabled likewise; an older gateway omits it ⇒ off (the
      // same fail-closed default `miniapp::enabled` uses).
      setMiniappEnabled(res.miniapp_enabled ?? false);
      // daily_digest_enabled / daily_digest_at come back structured too.
      setDailyDigest(res.daily_digest_enabled ?? false);
      setDailyDigestAt(res.daily_digest_at ?? '09:00');
      const tables = parseTomlSubset(raw);
      const v168 = readV168(tables);
      setExtra(v168);
      setSmTokenSet({
        onepassword_token: tomlSecretSet(tables, 'secret_manager', 'onepassword_token'),
        infisical_token: tomlSecretSet(tables, 'secret_manager', 'infisical_token'),
      });
      initialRef.current = snapshotSystem({
        name: general.match(/\bname\s*=\s*"([^"]*)"/)?.[1] ?? '',
        mdnsAdvertise: /\bmdns_advertise\s*=\s*true\b/.test(server),
        bind: bindVal,
        port: m(/\bport\s*=\s*(\d+)/) ?? '',
        healthInterval: m(/health_check_interval_seconds\s*=\s*(\d+)/) ?? '',
        cooldown: m(/cooldown_after_rate_limit_seconds\s*=\s*(\d+)/) ?? '',
        defaultAgent: m(/default_agent\s*=\s*"([^"]*)"/) ?? '',
        inferenceMode: m(/inference_mode\s*=\s*"(\w+)"/) ?? 'claude',
        logFormat: tomlStr(tables, 'logging', 'format', 'pretty') === 'json' ? 'json' : 'pretty',
        smBackend: SM_BACKENDS.includes(smRaw) ? smRaw : 'local',
        vaultAddr: m(/vault_addr\s*=\s*"([^"]*)"/) ?? '',
        vaultMount: m(/vault_mount\s*=\s*"([^"]*)"/) ?? '',
        origins: Array.isArray(ao) ? (ao.filter((v) => typeof v === 'string') as string[]) : [],
        noveltyGate: res.novelty_gate_enabled ?? true,
        miniappEnabled: res.miniapp_enabled ?? false,
        dailyDigest: res.daily_digest_enabled ?? false,
        dailyDigestAt: res.daily_digest_at ?? '09:00',
        extra: v168,
      });
    }).catch((e) => {
      console.warn('[api]', e);
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  // Add the draft entry to the allowlist (dedup, trim). Called by the Add button
  // and the Enter key. The gateway re-cleans each entry server-side, so we only
  // do light trimming here.
  const addOrigin = () => {
    const v = originDraft.trim();
    if (v === '') return;
    setOrigins((prev) => (prev.includes(v) ? prev : [...prev, v]));
    setOriginDraft('');
  };
  const removeOrigin = (target: string) => {
    setOrigins((prev) => prev.filter((o) => o !== target));
  };

  const handleSave = async () => {
    // Fail-closed on a custom bind IP before touching the network — instant
    // feedback; the gateway re-validates server-side regardless.
    if (bindMode === 'custom' && bind.trim() !== '' && !isValidIp(bind)) {
      toast.error(intl.formatMessage({ id: 'settings.system.bind.invalid' }));
      return;
    }
    // v1.68: the sandbox limits are validated as a whole server-side; catch
    // the two mistakes the form can make before sending anything.
    if (SANDBOX_NUMERIC_KEYS.some((k) => {
      const n = extra[k];
      return typeof n !== 'number' || !Number.isFinite(n) || n <= 0;
    })) {
      toast.error(intl.formatMessage({ id: 'settings.sandbox.invalidNumber' }));
      return;
    }
    if (sandboxSizesInvalid(extra)) {
      toast.error(intl.formatMessage({ id: 'settings.sandbox.sizeConstraint' }));
      return;
    }
    // v1.68 (plan principle 3): send only what changed since load. Secrets
    // are write-only and go out only when typed.
    const current = snapshotSystem({
      name: name.trim(), mdnsAdvertise, bind: bind.trim(), port: port.trim(),
      healthInterval: healthInterval.trim(), cooldown: cooldown.trim(), defaultAgent,
      inferenceMode, logFormat, smBackend, vaultAddr, vaultMount, origins, noveltyGate,
      miniappEnabled, dailyDigest,
      // An empty native time input must not be sent as "" — the gateway
      // rejects an unparseable daily_digest_at outright (fail-closed).
      dailyDigestAt: dailyDigestAt.trim() !== '' ? dailyDigestAt : '09:00',
      extra,
    });
    const payload = changedPayload(initialRef.current, current);
    if (authToken.trim() !== '') payload.auth_token = authToken.trim();
    const smSecrets: Record<string, string> = {};
    if (vaultToken.trim() !== '') smSecrets.vault_token = vaultToken.trim();
    if (smTokens.onepassword_token.trim() !== '') smSecrets.onepassword_token = smTokens.onepassword_token.trim();
    if (smTokens.infisical_token.trim() !== '') smSecrets.infisical_token = smTokens.infisical_token.trim();
    if (Object.keys(smSecrets).length > 0) {
      payload.secret_manager = { ...((payload.secret_manager as Record<string, unknown>) ?? {}), ...smSecrets };
    }
    if (Object.keys(payload).length === 0) {
      toast.info(intl.formatMessage({ id: 'settings.noChanges' }));
      return;
    }
    setSaving(true);
    setSaved(false);
    setSaveError(null);
    try {
      const res = await api.system.updateConfig(payload);
      initialRef.current = current;
      const restart = res?.restart_required ?? [];
      if (restart.length > 0) useRestartRequiredStore.getState().add(restart);
      setAuthToken('');
      setVaultToken('');
      setSmTokens({ onepassword_token: '', infisical_token: '' });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.warn('[api]', e);
      setSaveError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const saveErrorFields = saveError != null ? fieldsInError(saveError) : [];

  const agentOptions: SelectOption[] = [
    { value: '', label: intl.formatMessage({ id: 'settings.system.none' }), raw: '' },
    ...agents.map((a) => ({ value: a.name, label: a.display_name || a.name, raw: a.name })),
  ];
  const inferenceOptions: SelectOption[] = ['local', 'claude', 'hybrid'].map((v) => ({
    value: v, label: intl.formatMessage({ id: `settings.inferenceMode.${v}` }), raw: v,
  }));
  const logFormatOptions: SelectOption[] = ['pretty', 'json'].map((v) => ({
    value: v, label: intl.formatMessage({ id: `settings.logFormat.${v}` }), raw: v,
  }));
  const smOptions: SelectOption[] = SM_BACKENDS.map((v) => ({
    value: v, label: intl.formatMessage({ id: `settings.smBackend.${v}` }), raw: v,
  }));
  const bindModeOptions: SelectOption[] = (['loopback', 'lan', 'custom'] as BindMode[]).map((v) => ({
    value: v, label: intl.formatMessage({ id: `settings.system.bind.${v}` }), raw: v,
  }));

  const onBindModeChange = (m: string) => {
    setBindMode(m as BindMode);
    if (m === 'loopback') setBind('127.0.0.1');
    else if (m === 'lan') setBind('0.0.0.0');
    else setBind(''); // custom: user types the IP below
  };

  return (
    <div className="space-y-8">
      {loadError != null && (
        <ErrorState
          variant="inline"
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          onRetry={load}
        />
      )}

      {/* Server — display name + LAN binding/broadcast (mostly restart-required) */}
      <SettingsSection
        title={intl.formatMessage({ id: 'settings.system.server' })}
        description={intl.formatMessage({ id: 'settings.system.server.desc' })}
      >
        <SettingsCard>
          <SettingsRow
            label={intl.formatMessage({ id: 'settings.system.name' })}
            description={`${intl.formatMessage({ id: 'settings.system.name.help' })} ${intl.formatMessage({ id: 'settings.system.name.restartNote' })}`}
            tier="text"
          >
            <Input type="text" value={name} onChange={(e) => setName(e.target.value)} placeholder="Office Gateway" maxLength={64} />
          </SettingsRow>
        </SettingsCard>
        <DangerZone
          title={intl.formatMessage({ id: 'settings.system.dangerTitle' })}
          description={intl.formatMessage({ id: 'settings.system.serverRestart' })}
        >
          <SettingsCard>
            <RowSelect
              label={intl.formatMessage({ id: 'settings.system.bind' })}
              description={intl.formatMessage({ id: 'settings.system.bind.help' })}
              value={bindMode}
              onChange={onBindModeChange}
              options={bindModeOptions}
            />
            {bindMode === 'custom' && (
              <SettingsRow label={intl.formatMessage({ id: 'settings.system.bind.custom' })} tier="text">
                <Input type="text" value={bind} onChange={(e) => setBind(e.target.value)} placeholder="192.168.1.10" />
              </SettingsRow>
            )}
            <RowSwitch
              label={intl.formatMessage({ id: 'settings.system.mdns' })}
              description={intl.formatMessage({ id: 'settings.system.mdns.help' })}
              checked={mdnsAdvertise}
              onChange={setMdnsAdvertise}
            />
          </SettingsCard>
        </DangerZone>
      </SettingsSection>

      {/* Gateway network binding — restart-required, can lock you out */}
      <SettingsSection title={intl.formatMessage({ id: 'settings.system.gateway' })}>
        <DangerZone
          title={intl.formatMessage({ id: 'settings.system.dangerTitle' })}
          description={intl.formatMessage({ id: 'settings.system.dangerDesc' })}
        >
          <SettingsCard>
            <SettingsRow label={intl.formatMessage({ id: 'settings.system.port' })} description={intl.formatMessage({ id: 'settings.system.port.help' })} tier="select">
              <Input type="number" min={1} max={65535} value={port} onChange={(e) => setPort(e.target.value)} placeholder="3100" />
            </SettingsRow>
          </SettingsCard>
        </DangerZone>
        <SettingsCard>
          <SettingsRow label={intl.formatMessage({ id: 'settings.system.authToken' })} description={intl.formatMessage({ id: 'settings.system.writeOnly' })} tier="text">
            <Input type="password" value={authToken} onChange={(e) => setAuthToken(e.target.value)} placeholder="••••••••" autoComplete="off" />
          </SettingsRow>
        </SettingsCard>
      </SettingsSection>

      {/* Remote-access allowlist — non-secret, hot-applied (no restart) */}
      <SettingsSection
        title={intl.formatMessage({ id: 'settings.system.remoteAccess' })}
        description={intl.formatMessage({ id: 'settings.system.remoteAccess.desc' })}
      >
        <SettingsCard>
          <SettingsRow
            label={intl.formatMessage({ id: 'settings.system.remoteAccess.add' })}
            description={intl.formatMessage({ id: 'settings.system.remoteAccess.help' })}
            tier="text"
          >
            <div className="flex gap-2">
              <Input
                type="text"
                value={originDraft}
                onChange={(e) => setOriginDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !isImeComposing(e)) {
                    e.preventDefault();
                    addOrigin();
                  }
                }}
                placeholder="dash.example.com"
                aria-label={intl.formatMessage({ id: 'settings.system.remoteAccess.add' })}
              />
              <Button
                type="button"
                variant="secondary"
                size="sm"
                onClick={addOrigin}
                disabled={originDraft.trim() === ''}
              >
                <Plus className="size-4" />
                {intl.formatMessage({ id: 'common.add' })}
              </Button>
            </div>
          </SettingsRow>
          <div className="px-4 py-3.5">
            {origins.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'settings.system.remoteAccess.empty' })}
              </p>
            ) : (
              <ul className="flex flex-wrap gap-2" aria-label={intl.formatMessage({ id: 'settings.system.remoteAccess' })}>
                {origins.map((o) => (
                  <li
                    key={o}
                    className="inline-flex items-center gap-1.5 rounded-4xl border border-border bg-secondary px-2.5 py-1 text-xs text-secondary-foreground"
                  >
                    <span className="max-w-[16rem] truncate">{o}</span>
                    <button
                      type="button"
                      onClick={() => removeOrigin(o)}
                      className="rounded-full text-muted-foreground transition-colors hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
                      aria-label={intl.formatMessage({ id: 'settings.system.remoteAccess.remove' }, { origin: o })}
                    >
                      <X className="size-3.5" />
                    </button>
                  </li>
                ))}
              </ul>
            )}
            <p className="mt-2 text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'settings.system.remoteAccess.builtin' })}
            </p>
          </div>
        </SettingsCard>
      </SettingsSection>

      {/* Rotation */}
      <SettingsSection title={intl.formatMessage({ id: 'settings.system.rotation' })}>
        <SettingsCard>
          <SettingsRow label={intl.formatMessage({ id: 'settings.system.healthInterval' })} description={`${intl.formatMessage({ id: 'settings.system.healthInterval.help' })} ${intl.formatMessage({ id: 'settings.restartNote' })}`} tier="select">
            <Input type="number" min={1} max={86400} value={healthInterval} onChange={(e) => setHealthInterval(e.target.value)} placeholder="60" />
          </SettingsRow>
          <SettingsRow label={intl.formatMessage({ id: 'settings.system.cooldown' })} description={intl.formatMessage({ id: 'settings.system.cooldown.help' })} tier="select">
            <Input type="number" min={1} max={86400} value={cooldown} onChange={(e) => setCooldown(e.target.value)} placeholder="120" />
          </SettingsRow>
        </SettingsCard>
      </SettingsSection>

      {/* General + Logging */}
      <SettingsSection title={intl.formatMessage({ id: 'settings.system.general' })}>
        <SettingsCard>
          <RowSelect
            label={intl.formatMessage({ id: 'settings.system.defaultAgent' })}
            description={intl.formatMessage({ id: 'settings.system.defaultAgent.help' })}
            value={defaultAgent}
            onChange={setDefaultAgent}
            options={agentOptions}
          />
          <RowSelect
            label={intl.formatMessage({ id: 'settings.system.inferenceMode' })}
            description={intl.formatMessage({ id: 'settings.system.inferenceMode.help' })}
            value={inferenceMode}
            onChange={setInferenceMode}
            options={inferenceOptions}
          />
          <RowSelect
            label={intl.formatMessage({ id: 'settings.system.logFormat' })}
            description={`${intl.formatMessage({ id: 'settings.system.logFormat.help' })} ${intl.formatMessage({ id: 'settings.restartNote' })}`}
            value={logFormat}
            onChange={setLogFormat}
            options={logFormatOptions}
          />
        </SettingsCard>
      </SettingsSection>

      {/* Memory — B1 write-time dedup gate */}
      <SettingsSection title={intl.formatMessage({ id: 'settings.system.memory' })}>
        <SettingsCard>
          <RowSwitch
            label={intl.formatMessage({ id: 'settings.system.noveltyGate' })}
            description={intl.formatMessage({ id: 'settings.system.noveltyGate.help' })}
            checked={noveltyGate}
            onChange={setNoveltyGate}
          />
        </SettingsCard>
      </SettingsSection>

      {/* v1.68 (W2) — memory trust guard, tracing, GitHub tools, A2A trust */}
      <TrustAndObservabilitySection values={extra} set={setExtraKey} otelCompiled={otelCompiled} isAdmin={isAdmin} />

      {/* v1.68 (W2) — local-file roots for file_read / csv_read / xlsx_read */}
      <FilesRootsSection values={extra} set={setExtraKey} />

      {/* v1.68 (W2) — task sandbox + computer-use image */}
      <SandboxSection values={extra} set={setExtraKey} />

      {/* S20 — Telegram Mini App approval screen. Off by default; needs an
          https dashboard URL to render a button at all. */}
      <SettingsSection title={intl.formatMessage({ id: 'settings.system.miniapp' })}>
        <SettingsCard>
          <RowSwitch
            label={intl.formatMessage({ id: 'settings.system.miniapp.enabled' })}
            description={intl.formatMessage({ id: 'settings.system.miniapp.enabled.help' })}
            checked={miniappEnabled}
            onChange={setMiniappEnabled}
          />
        </SettingsCard>
      </SettingsSection>

      {/* Daily digest — W2-8, the second of the two notification channels
          (event-driven pushes above, a scheduled roll-up here). Off by
          default; "無事不寄" — a quiet day sends nothing at all. */}
      <SettingsSection
        title={intl.formatMessage({ id: 'settings.system.dailyDigest' })}
        description={intl.formatMessage({ id: 'settings.system.dailyDigest.desc' })}
      >
        <SettingsCard>
          <RowSwitch
            label={intl.formatMessage({ id: 'settings.system.dailyDigest.enabled' })}
            description={intl.formatMessage({ id: 'settings.system.dailyDigest.enabled.help' })}
            checked={dailyDigest}
            onChange={setDailyDigest}
          />
          {dailyDigest && (
            <SettingsRow
              label={intl.formatMessage({ id: 'settings.system.dailyDigest.at' })}
              description={intl.formatMessage({ id: 'settings.system.dailyDigest.at.help' })}
              tier="select"
            >
              <Input
                type="time"
                value={dailyDigestAt}
                onChange={(e) => setDailyDigestAt(e.target.value)}
                aria-label={intl.formatMessage({ id: 'settings.system.dailyDigest.at' })}
              />
            </SettingsRow>
          )}
        </SettingsCard>
      </SettingsSection>

      {/* Secret manager — advanced */}
      <SettingsSection title={intl.formatMessage({ id: 'settings.system.secrets' })}>
        <AdvancedSection storageKey="settings.system.secrets" label={intl.formatMessage({ id: 'settings.system.vaultSection' })}>
          <SettingsCard>
            <RowSelect
              label={intl.formatMessage({ id: 'settings.system.smBackend' })}
              value={smBackend}
              onChange={setSmBackend}
              options={smOptions}
            />
            {smBackend === 'vault' && (
              <>
                <SettingsRow label={intl.formatMessage({ id: 'settings.system.vaultAddr' })} tier="text">
                  <Input type="text" value={vaultAddr} onChange={(e) => setVaultAddr(e.target.value)} placeholder="https://vault:8200" />
                </SettingsRow>
                <SettingsRow label={intl.formatMessage({ id: 'settings.system.vaultMount' })} tier="text">
                  <Input type="text" value={vaultMount} onChange={(e) => setVaultMount(e.target.value)} placeholder="secret" />
                </SettingsRow>
                <SettingsRow label={intl.formatMessage({ id: 'settings.system.vaultToken' })} description={intl.formatMessage({ id: 'settings.system.writeOnly' })} tier="text">
                  <Input type="password" value={vaultToken} onChange={(e) => setVaultToken(e.target.value)} placeholder="••••••••" autoComplete="off" />
                </SettingsRow>
              </>
            )}
            {/* v1.68: onepassword / infisical could be selected but never
                configured — every secret:// lookup then failed closed. */}
            <SecretBackendFields
              backend={smBackend}
              values={extra}
              set={setExtraKey}
              tokens={smTokens}
              setToken={(k, v) => setSmTokens((prev) => ({ ...prev, [k]: v }))}
              tokenSet={smTokenSet}
            />
          </SettingsCard>
        </AdvancedSection>
      </SettingsSection>

      {saveError != null && (
        <ErrorState
          variant="inline"
          error={saveError}
          title={intl.formatMessage({ id: 'errorState.manage.saveFailed' })}
          description={
            <>
              {intl.formatMessage({ id: 'errorState.manage.saveFailedHint' })}
              {saveErrorFields.length > 0 && (
                <>
                  {' '}
                  {intl.formatMessage(
                    { id: 'errorState.manage.fieldHint' },
                    {
                      fields: saveErrorFields
                        .map((id) => intl.formatMessage({ id }))
                        .join('、'),
                    },
                  )}
                </>
              )}
            </>
          }
          onRetry={() => void handleSave()}
        />
      )}
      <div className="flex items-center justify-end gap-3">
        <SettingsSaveState
          status={saving ? 'saving' : saved ? 'saved' : 'idle'}
          savingLabel={intl.formatMessage({ id: 'common.saving' })}
          savedLabel={intl.formatMessage({ id: 'settings.general.saved' })}
        />
        <Button variant="brand" size="sm" onClick={handleSave} disabled={saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </div>
  );
}

interface SystemSnapshotInput {
  name: string;
  mdnsAdvertise: boolean;
  bind: string;
  port: string;
  healthInterval: string;
  cooldown: string;
  defaultAgent: string;
  inferenceMode: string;
  logFormat: string;
  smBackend: string;
  vaultAddr: string;
  vaultMount: string;
  origins: string[];
  noveltyGate: boolean;
  miniappEnabled: boolean;
  dailyDigest: boolean;
  dailyDigestAt: string;
  extra: FlatValues;
}

/** Flatten the form into `system.update_config` param paths. Blank numeric
 *  fields are left out entirely (never sent as 0 / NaN), exactly as before. */
function snapshotSystem(v: SystemSnapshotInput): FlatValues {
  const out: FlatValues = {
    name: v.name,
    mdns_advertise: v.mdnsAdvertise,
    default_agent: v.defaultAgent,
    inference_mode: v.inferenceMode,
    log_format: v.logFormat,
    'secret_manager.backend': v.smBackend,
    'secret_manager.vault_addr': v.vaultAddr,
    'secret_manager.vault_mount': v.vaultMount,
    allowed_origins: v.origins,
    novelty_gate_enabled: v.noveltyGate,
    miniapp_enabled: v.miniappEnabled,
    daily_digest: v.dailyDigest,
    daily_digest_at: v.dailyDigestAt,
    ...v.extra,
  };
  if (v.bind !== '') out.bind = v.bind;
  if (v.port !== '') out.port = Number(v.port);
  if (v.healthInterval !== '') out.health_check_interval_seconds = Number(v.healthInterval);
  if (v.cooldown !== '') out.cooldown_after_rate_limit_seconds = Number(v.cooldown);
  return out;
}
