import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
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
  const smOptions: SelectOption[] = ['env', 'vault', 'config', 'keychain'].map((v) => ({
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
