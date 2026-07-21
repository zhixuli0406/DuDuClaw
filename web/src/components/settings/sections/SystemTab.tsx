import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { Plus, X } from 'lucide-react';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import { isImeComposing } from '@/lib/keyboard';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Input,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
} from '@/components/mds';
import { AdvancedSection, DangerZone, type SelectOption } from '@/components/settings/controls';
import { RowSelect } from '@/pages/agent-form/form-rows';

// Secret-manager backends the gateway's system.update_config accepts.
const SM_BACKENDS = ['local', 'vault', 'env', 'onepassword', 'infisical'];

// ── G — System tab (gateway / rotation / general / logging / secret_manager) ──

export function SystemTab() {
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
  const [smBackend, setSmBackend] = useState('local');
  const [vaultAddr, setVaultAddr] = useState('');
  const [vaultMount, setVaultMount] = useState('');
  const [vaultToken, setVaultToken] = useState('');
  // [gateway] allowed_origins — remote-access allowlist (chips + draft input).
  const [origins, setOrigins] = useState<string[]>([]);
  const [originDraft, setOriginDraft] = useState('');

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
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

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
      // Always send the current allowlist so a save reflects add/remove edits.
      // Empty array = loopback-only (the default). Hot-applied server-side.
      payload.allowed_origins = origins;

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

  return (
    <div className="space-y-8">
      {/* Gateway network binding — restart-required, can lock you out */}
      <SettingsSection title={intl.formatMessage({ id: 'settings.system.gateway' })}>
        <DangerZone
          title={intl.formatMessage({ id: 'settings.system.dangerTitle' })}
          description={intl.formatMessage({ id: 'settings.system.dangerDesc' })}
        >
          <SettingsCard>
            <SettingsRow label={intl.formatMessage({ id: 'settings.system.bind' })} description={intl.formatMessage({ id: 'settings.system.bind.help' })} tier="text">
              <Input type="text" value={bind} onChange={(e) => setBind(e.target.value)} placeholder="0.0.0.0" />
            </SettingsRow>
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
          <SettingsRow label={intl.formatMessage({ id: 'settings.system.healthInterval' })} description={intl.formatMessage({ id: 'settings.system.healthInterval.help' })} tier="select">
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
            description={intl.formatMessage({ id: 'settings.system.logFormat.help' })}
            value={logFormat}
            onChange={setLogFormat}
            options={logFormatOptions}
          />
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
          </SettingsCard>
        </AdvancedSection>
      </SettingsSection>

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
