import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import { FormField, inputClass } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import { Card, Button } from '@/components/ui';
import { AdvancedSection, DangerZone, OptionSelect, SettingField, type SelectOption } from '@/components/settings/controls';

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
    <div className="space-y-6">
      <p className="rounded-lg bg-stone-500/5 px-4 py-3 text-sm text-stone-500 dark:bg-white/5 dark:text-stone-400">
        {intl.formatMessage({ id: 'settings.system.desc' })}
      </p>

      {/* Gateway network binding — restart-required, can lock you out */}
      <Card title={intl.formatMessage({ id: 'settings.system.gateway' })} bodyClassName="space-y-4">
        <DangerZone
          title={intl.formatMessage({ id: 'settings.system.dangerTitle' })}
          description={intl.formatMessage({ id: 'settings.system.dangerDesc' })}
        >
          <div className="grid grid-cols-2 gap-3">
            <SettingField label={intl.formatMessage({ id: 'settings.system.bind' })} help={intl.formatMessage({ id: 'settings.system.bind.help' })}>
              <input type="text" value={bind} onChange={(e) => setBind(e.target.value)} placeholder="0.0.0.0" className={inputClass} />
            </SettingField>
            <SettingField label={intl.formatMessage({ id: 'settings.system.port' })} help={intl.formatMessage({ id: 'settings.system.port.help' })}>
              <input type="number" min={1} max={65535} value={port} onChange={(e) => setPort(e.target.value)} placeholder="3100" className={inputClass} />
            </SettingField>
          </div>
        </DangerZone>
        <FormField label={intl.formatMessage({ id: 'settings.system.authToken' })} hint={intl.formatMessage({ id: 'settings.system.writeOnly' })}>
          <input type="password" value={authToken} onChange={(e) => setAuthToken(e.target.value)} placeholder="••••••••" className={inputClass} autoComplete="off" />
        </FormField>
      </Card>

      {/* Rotation */}
      <Card title={intl.formatMessage({ id: 'settings.system.rotation' })} bodyClassName="space-y-4">
        <div className="grid grid-cols-2 gap-3">
          <SettingField label={intl.formatMessage({ id: 'settings.system.healthInterval' })} help={intl.formatMessage({ id: 'settings.system.healthInterval.help' })}>
            <input type="number" min={1} max={86400} value={healthInterval} onChange={(e) => setHealthInterval(e.target.value)} placeholder="60" className={inputClass} />
          </SettingField>
          <SettingField label={intl.formatMessage({ id: 'settings.system.cooldown' })} help={intl.formatMessage({ id: 'settings.system.cooldown.help' })}>
            <input type="number" min={1} max={86400} value={cooldown} onChange={(e) => setCooldown(e.target.value)} placeholder="120" className={inputClass} />
          </SettingField>
        </div>
      </Card>

      {/* General + Logging */}
      <Card title={intl.formatMessage({ id: 'settings.system.general' })} bodyClassName="space-y-4">
        <SettingField label={intl.formatMessage({ id: 'settings.system.defaultAgent' })} help={intl.formatMessage({ id: 'settings.system.defaultAgent.help' })}>
          <OptionSelect value={defaultAgent} onChange={setDefaultAgent} options={agentOptions} showRaw={false} />
        </SettingField>
        <div className="grid grid-cols-2 gap-3">
          <SettingField label={intl.formatMessage({ id: 'settings.system.inferenceMode' })} help={intl.formatMessage({ id: 'settings.system.inferenceMode.help' })}>
            <OptionSelect value={inferenceMode} onChange={setInferenceMode} options={inferenceOptions} />
          </SettingField>
          <SettingField label={intl.formatMessage({ id: 'settings.system.logFormat' })} help={intl.formatMessage({ id: 'settings.system.logFormat.help' })}>
            <OptionSelect value={logFormat} onChange={setLogFormat} options={logFormatOptions} />
          </SettingField>
        </div>
      </Card>

      {/* Secret manager — advanced */}
      <Card title={intl.formatMessage({ id: 'settings.system.secrets' })} bodyClassName="space-y-4">
        <AdvancedSection storageKey="settings.system.secrets" label={intl.formatMessage({ id: 'settings.system.vaultSection' })}>
          <SettingField label={intl.formatMessage({ id: 'settings.system.smBackend' })}>
            <OptionSelect value={smBackend} onChange={setSmBackend} options={smOptions} />
          </SettingField>
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
        </AdvancedSection>
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
