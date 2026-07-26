import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type OdooStatus,
  type OdooAgentConfig,
  type OdooAgentConfigSet,
  type OdooDiscoverSchemaResult,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  CheckCircle,
  XCircle,
  Loader2,
  RefreshCw,
  Save,
  AlertTriangle,
  Database,
  Search,
} from 'lucide-react';
import {
  Button,
  Badge,
  Input,
  Checkbox,
  Spinner,
  SettingsSection,
  SettingsCard,
} from '@/components/mds';
import { RowText, RowNumber, RowSwitch, RowSelect, FieldBlock } from '@/pages/agent-form/form-rows';
import type { SelectOption } from '@/components/settings/controls';

const FEATURE_MODULES = ['crm', 'sale', 'inventory', 'accounting', 'project', 'hr'] as const;
type FeatureKey = (typeof FEATURE_MODULES)[number];

/**
 * OdooPage — Odoo ERP tab of `/manage/integrations` (MDS Settings surface).
 * A slim header (description + connection status + Save) over Settings-式 cards:
 * connection credentials, feature modules, sync (polling/webhook) and a
 * per-AI-employee credential override. The test-before-save flow and the
 * write-only credential semantics are preserved verbatim — same `odoo.*` RPCs.
 */
export function OdooPage() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  // Connection config
  const [url, setUrl] = useState('');
  const [db, setDb] = useState('');
  const [protocol, setProtocol] = useState('jsonrpc');
  const [authMethod, setAuthMethod] = useState('api_key');
  const [username, setUsername] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [password, setPassword] = useState('');

  // Polling / Webhook — defaults match OdooConfig::default() in config.rs
  const [pollEnabled, setPollEnabled] = useState(true);
  const [pollInterval, setPollInterval] = useState('60');
  const [pollModels, setPollModels] = useState('crm.lead,sale.order');
  const [webhookEnabled, setWebhookEnabled] = useState(false);
  const [webhookSecret, setWebhookSecret] = useState('');
  const [globalUnblockModels, setGlobalUnblockModels] = useState('');

  // Feature toggles
  const [features, setFeatures] = useState<Record<FeatureKey, boolean>>({
    crm: true,
    sale: true,
    inventory: true,
    accounting: true,
    project: false,
    hr: false,
  });

  // UI state
  const [status, setStatus] = useState<OdooStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Schema discovery ("解析資料庫")
  const [discovering, setDiscovering] = useState(false);
  const [schema, setSchema] = useState<OdooDiscoverSchemaResult | null>(null);
  const [schemaError, setSchemaError] = useState<string | null>(null);
  const [schemaFilter, setSchemaFilter] = useState('');
  const [pickedModels, setPickedModels] = useState<Set<string>>(new Set());

  const loadConfig = useCallback(async () => {
    setLoading(true);
    try {
      const [statusRes, configRes] = await Promise.all([
        api.odoo.status(),
        api.odoo.config(),
      ]);
      setStatus(statusRes);
      if (configRes) {
        setUrl(configRes.url ?? '');
        setDb(configRes.db ?? '');
        setProtocol(configRes.protocol ?? 'jsonrpc');
        setAuthMethod(configRes.auth_method ?? 'api_key');
        setUsername(configRes.username ?? '');
        setPollEnabled(configRes.poll_enabled ?? true);
        setPollInterval(String(configRes.poll_interval_seconds ?? 60));
        setPollModels((configRes.poll_models ?? []).join(','));
        setWebhookEnabled(configRes.webhook_enabled ?? false);
        setGlobalUnblockModels((configRes.unblock_models ?? []).join(', '));
        setFeatures({
          crm: configRes.features_crm ?? true,
          sale: configRes.features_sale ?? true,
          inventory: configRes.features_inventory ?? true,
          accounting: configRes.features_accounting ?? true,
          project: configRes.features_project ?? false,
          hr: configRes.features_hr ?? false,
        });
      }
    } catch (e) {
      // Silently handle feature-gate errors (not licensed); show other errors
      const msg = e instanceof Error ? e.message : '';
      if (!msg.includes('Feature requires upgrade')) {
        setError(t('common.error'));
      }
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    loadConfig();
    return () => {
      if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
    };
  }, [loadConfig]);

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    setError(null);
    try {
      await api.odoo.configure({
        url: url.trim(),
        db: db.trim(),
        protocol,
        auth_method: authMethod,
        username: username.trim(),
        api_key: authMethod === 'api_key' ? apiKey : undefined,
        password: authMethod === 'password' ? password : undefined,
        poll_enabled: pollEnabled,
        poll_interval_seconds: Math.max(60, Math.min(86400, Number(pollInterval) || 300)),
        poll_models: pollModels.split(',').map((s) => s.trim()).filter(Boolean),
        webhook_enabled: webhookEnabled,
        webhook_secret: webhookSecret || undefined,
        unblock_models: globalUnblockModels
          .split(/[,\s]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        features_crm: features.crm,
        features_sale: features.sale,
        features_inventory: features.inventory,
        features_accounting: features.accounting,
        features_project: features.project,
        features_hr: features.hr,
      });
      setSaved(true);
      if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
      savedTimerRef.current = setTimeout(() => setSaved(false), 3000);
      // Clear credential fields after successful save (backend stores encrypted)
      setApiKey('');
      setPassword('');
      setWebhookSecret('');
      await loadConfig();
    } catch (e) {
      const detail = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
      setError(detail ? `${t('odoo.saveFailed')}: ${detail}` : t('odoo.saveFailed'));
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      // Test with the **current form values** — backend treats this as a
      // transient test (nothing written to disk). If the credential field is
      // empty (e.g. after a save where the masked input was cleared), the
      // backend falls back to the stored credential.
      const res = await api.odoo.test({
        url: url.trim(),
        db: db.trim(),
        protocol,
        auth_method: authMethod,
        username: username.trim(),
        api_key: authMethod === 'api_key' && apiKey ? apiKey : undefined,
        password: authMethod === 'password' && password ? password : undefined,
      });
      setTestResult({ ok: res.success, message: res.message });
    } catch (e) {
      const detail = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
      setTestResult({
        ok: false,
        message: detail ? `${t('odoo.testFailed')}: ${detail}` : t('odoo.testFailed'),
      });
    } finally {
      setTesting(false);
    }
  };

  const handleDiscover = async () => {
    setDiscovering(true);
    setSchemaError(null);
    try {
      const res = await api.odoo.discoverSchema();
      if (!res.success) {
        setSchema(null);
        setSchemaError(res.message || t('odoo.schema.failed'));
        return;
      }
      setSchema(res);
      setPickedModels(new Set());
      setSchemaFilter('');
    } catch (e) {
      const detail = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
      setSchema(null);
      setSchemaError(detail ? `${t('odoo.schema.failed')}: ${detail}` : t('odoo.schema.failed'));
    } finally {
      setDiscovering(false);
    }
  };

  const toggleModel = (model: string) => {
    setPickedModels((prev) => {
      const next = new Set(prev);
      if (next.has(model)) next.delete(model);
      else next.add(model);
      return next;
    });
  };

  const filteredModels = (schema?.models ?? []).filter((m) => {
    const q = schemaFilter.trim().toLowerCase();
    if (!q) return true;
    return m.model.toLowerCase().includes(q) || m.name.toLowerCase().includes(q);
  });

  const protocolOptions: SelectOption[] = [
    { value: 'jsonrpc', label: 'JSON-RPC' },
    { value: 'xmlrpc', label: 'XML-RPC' },
  ];
  const authOptions: SelectOption[] = [
    { value: 'api_key', label: t('odoo.authApiKey') },
    { value: 'password', label: t('odoo.authPassword') },
  ];

  if (loading) {
    return (
      <div className="flex items-center justify-center py-20" role="status" aria-live="polite">
        <Spinner className="size-6 text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Slim tab header — description + status left, Save right. */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-3">
          <p className="text-sm text-muted-foreground">{t('odoo.subtitle')}</p>
          {status && (
            <Badge
              className={cn(
                status.connected
                  ? 'border-success/30 bg-success/10 text-success'
                  : 'border-border text-muted-foreground'
              )}
            >
              {status.connected ? <CheckCircle /> : <XCircle />}
              {status.connected ? t('odoo.connected') : t('odoo.disconnected')}
              {status.edition && <span className="opacity-70">({status.edition})</span>}
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-3">
          {error && <span className="text-sm text-destructive">{error}</span>}
          {saved && (
            <span className="inline-flex items-center gap-1.5 text-sm font-medium text-success">
              <CheckCircle className="size-4" />
              {t('common.saved')}
            </span>
          )}
          <Button variant="brand" size="sm" onClick={handleSave} disabled={saving || !url.trim()}>
            {saving ? <Loader2 className="animate-spin" /> : <Save />}
            {saving ? t('common.saving') : t('common.save')}
          </Button>
        </div>
      </div>

      {status && !status.connected && status.error && (
        <p className="text-xs text-destructive">{status.error}</p>
      )}

      {/* Connection settings */}
      <SettingsSection title={t('odoo.connection')}>
        <SettingsCard>
          <RowText label={t('odoo.url')} value={url} onChange={setUrl} placeholder="https://mycompany.odoo.com" tier="text" />
          <RowText label={t('odoo.db')} value={db} onChange={setDb} placeholder="mycompany" tier="text" />
          <RowSelect label={t('odoo.protocol')} value={protocol} onChange={setProtocol} options={protocolOptions} />
          <RowSelect label={t('odoo.authMethod')} value={authMethod} onChange={setAuthMethod} options={authOptions} />
          <RowText label={t('odoo.username')} value={username} onChange={setUsername} placeholder="admin@mycompany.com" tier="text" />
          {authMethod === 'api_key' ? (
            <RowText
              label={t('odoo.apiKey')}
              description={t('odoo.apiKeyHint')}
              value={apiKey}
              onChange={setApiKey}
              type="password"
              placeholder="••••••••"
              autoComplete="off"
              tier="text"
            />
          ) : (
            <RowText
              label={t('odoo.password')}
              value={password}
              onChange={setPassword}
              type="password"
              placeholder="••••••••"
              autoComplete="off"
              tier="text"
            />
          )}
        </SettingsCard>

        {/* Test-before-save — tests the current form values (transient). */}
        <div className="flex flex-wrap items-center gap-3">
          <Button
            variant="outline"
            size="sm"
            onClick={handleTest}
            disabled={testing || !url.trim() || !db.trim() || saving}
            title={!status?.connected ? t('odoo.testHint') : undefined}
          >
            {testing ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            {t('odoo.testConnection')}
          </Button>
          {/* Schema discovery — introspect models/fields + write to the wiki. */}
          <Button
            variant="outline"
            size="sm"
            onClick={handleDiscover}
            disabled={discovering || !url.trim() || !db.trim() || saving}
            title={t('odoo.schema.hint')}
          >
            {discovering ? <Loader2 className="animate-spin" /> : <Database />}
            {t('odoo.schema.discover')}
          </Button>
          {testResult && (
            <span
              className={cn(
                'inline-flex items-center gap-1.5 text-sm font-medium',
                testResult.ok ? 'text-success' : 'text-destructive'
              )}
            >
              {testResult.ok ? <CheckCircle className="size-4" /> : <AlertTriangle className="size-4" />}
              {testResult.message}
            </span>
          )}
          {schemaError && (
            <span className="inline-flex items-center gap-1.5 text-sm font-medium text-destructive">
              <AlertTriangle className="size-4" />
              {schemaError}
            </span>
          )}
        </div>

        {/* Schema discovery results — searchable, checkable model list. */}
        {schema && (
          <div className="space-y-3 rounded-lg border border-surface-border p-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-sm font-medium">
                {t('odoo.schema.found')
                  .replace('{count}', String(schema.total_models ?? 0))
                  .replace('{shown}', String(schema.models?.length ?? 0))}
                {schema.truncated ? ` ${t('odoo.schema.truncated')}` : ''}
              </p>
              {schema.wiki_written ? (
                <span className="inline-flex items-center gap-1.5 text-xs text-success">
                  <CheckCircle className="size-3.5" />
                  {t('odoo.schema.wikiWritten')}
                </span>
              ) : (
                schema.wiki_note && (
                  <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
                    <AlertTriangle className="size-3.5" />
                    {t('odoo.schema.wikiSkipped')}
                  </span>
                )
              )}
            </div>

            <div className="relative">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                className="pl-8"
                placeholder={t('odoo.schema.filter')}
                value={schemaFilter}
                onChange={(e) => setSchemaFilter(e.target.value)}
              />
            </div>

            <div className="max-h-72 overflow-y-auto rounded-md border border-surface-border">
              {filteredModels.length === 0 ? (
                <p className="p-3 text-center text-sm text-muted-foreground">{t('common.noData')}</p>
              ) : (
                <ul className="divide-y divide-surface-border">
                  {filteredModels.map((m) => (
                    <li key={m.model}>
                      <label className="flex cursor-pointer items-center gap-3 px-3 py-2 text-sm hover:bg-surface-hover">
                        <Checkbox
                          checked={pickedModels.has(m.model)}
                          onCheckedChange={() => toggleModel(m.model)}
                          aria-label={m.model}
                        />
                        <span className="font-mono text-xs">{m.model}</span>
                        {m.custom && (
                          <Badge className="border-brand/30 bg-brand/10 text-brand">
                            {t('odoo.schema.custom')}
                          </Badge>
                        )}
                        <span className="truncate text-muted-foreground">{m.name}</span>
                        <span className="ml-auto shrink-0 text-xs text-muted-foreground">
                          {t('odoo.schema.fieldCount').replace('{n}', String(m.field_count))}
                        </span>
                      </label>
                    </li>
                  ))}
                </ul>
              )}
            </div>
            <p className="text-xs text-muted-foreground">{t('odoo.schema.pickHint')}</p>
          </div>
        )}
      </SettingsSection>

      {/* Feature modules */}
      <SettingsSection title={t('odoo.features')} description={t('odoo.featuresDesc')}>
        <SettingsCard>
          {FEATURE_MODULES.map((key) => (
            <RowSwitch
              key={key}
              label={t(`odoo.feature.${key}`)}
              description={t(`odoo.feature.${key}.desc`)}
              checked={features[key]}
              onChange={(v) => setFeatures((prev) => ({ ...prev, [key]: v }))}
            />
          ))}
        </SettingsCard>
      </SettingsSection>

      {/* Polling & Webhook */}
      <SettingsSection title={t('odoo.sync')}>
        <SettingsCard>
          <RowSwitch label={t('odoo.pollEnabled')} checked={pollEnabled} onChange={setPollEnabled} />
          {pollEnabled && (
            <>
              <RowNumber
                label={t('odoo.pollInterval')}
                description={t('odoo.pollIntervalHint')}
                value={Number(pollInterval) || 60}
                min={60}
                max={86400}
                onChange={(v) => setPollInterval(String(v))}
              />
              <RowText
                label={t('odoo.pollModels')}
                description={t('odoo.pollModelsHint')}
                value={pollModels}
                onChange={setPollModels}
                placeholder="crm.lead,sale.order"
                tier="text"
              />
            </>
          )}
          <RowSwitch label={t('odoo.webhookEnabled')} checked={webhookEnabled} onChange={setWebhookEnabled} />
          {webhookEnabled && (
            <RowText
              label={t('odoo.webhookSecret')}
              description={t('odoo.webhookSecretHint')}
              value={webhookSecret}
              onChange={setWebhookSecret}
              type="password"
              placeholder="••••••••"
              tier="text"
            />
          )}
        </SettingsCard>
      </SettingsSection>

      {/* Global model access (security unblock list) */}
      <SettingsSection title={t('odoo.access')}>
        <SettingsCard>
          <RowText
            label={t('odoo.unblockModels')}
            description={t('odoo.unblockModelsHint')}
            value={globalUnblockModels}
            onChange={setGlobalUnblockModels}
            placeholder="res.partner, x_custom_model"
            tier="text"
          />
        </SettingsCard>
      </SettingsSection>

      {/* Per-agent credential override */}
      <AgentOdooOverride pickedModels={Array.from(pickedModels)} />
    </div>
  );
}

/**
 * Per-AI-staff Odoo credential override. Pick a staffer, see whether they have
 * an override (or inherit the global config), then edit URL / DB / user /
 * credentials + permission allowlists. api_key / password are write-only:
 * blank keeps the stored secret, the "clear" toggle wipes it. A separate test
 * button verifies the staffer's effective connection.
 */
function AgentOdooOverride({ pickedModels = [] }: { pickedModels?: string[] }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selected, setSelected] = useState('');
  const [cfg, setCfg] = useState<OdooAgentConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);

  // Editable form
  const [profile, setProfile] = useState('');
  const [url, setUrl] = useState('');
  const [db, setDb] = useState('');
  const [username, setUsername] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [password, setPassword] = useState('');
  const [clearApiKey, setClearApiKey] = useState(false);
  const [clearPassword, setClearPassword] = useState(false);
  const [allowedModels, setAllowedModels] = useState('');
  const [unblockModels, setUnblockModels] = useState('');
  const [allowedActions, setAllowedActions] = useState('');
  const [companyIds, setCompanyIds] = useState('');

  // Load agent roster once
  useEffect(() => {
    api.agents
      .list()
      .then((res) => {
        const list = res?.agents ?? [];
        setAgents(list);
        if (list.length > 0) setSelected((prev) => prev || list[0].name);
      })
      .catch((e) => {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
  }, [intl]);

  const applyCfg = useCallback((c: OdooAgentConfig | null) => {
    setCfg(c);
    setProfile(c?.profile ?? '');
    setUrl(c?.url ?? '');
    setDb(c?.db ?? '');
    setUsername(c?.username ?? '');
    setApiKey('');
    setPassword('');
    setClearApiKey(false);
    setClearPassword(false);
    setAllowedModels((c?.allowed_models ?? []).join(', '));
    setUnblockModels((c?.unblock_models ?? []).join(', '));
    setAllowedActions((c?.allowed_actions ?? []).join(', '));
    setCompanyIds((c?.company_ids ?? []).join(', '));
  }, []);

  // Load override config when the selected staffer changes
  useEffect(() => {
    if (!selected) return;
    setLoading(true);
    setError(null);
    setTestResult(null);
    setSaved(false);
    api.odoo
      .agentConfigGet(selected)
      .then(applyCfg)
      .catch((e) => {
        console.warn('[api]', e);
        setCfg(null);
        setError(t('common.error'));
      })
      .finally(() => setLoading(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected, applyCfg]);

  const parseList = (s: string) =>
    s.split(',').map((x) => x.trim()).filter(Boolean);

  const handleSave = async () => {
    if (!selected) return;
    setSaving(true);
    setSaved(false);
    setError(null);
    try {
      const payload: OdooAgentConfigSet = {
        agent_id: selected,
        profile: profile.trim() || undefined,
        url: url.trim() || undefined,
        db: db.trim() || undefined,
        user: username.trim() || undefined,
        allowed_models: parseList(allowedModels),
        unblock_models: parseList(unblockModels),
        allowed_actions: parseList(allowedActions),
        company_ids: parseList(companyIds)
          .map((n) => Number(n))
          .filter((n) => Number.isFinite(n)),
      };
      // api_key / password: clear toggle wins, else only send when typed.
      if (clearApiKey) payload.api_key = '';
      else if (apiKey) payload.api_key = apiKey;
      if (clearPassword) payload.password = '';
      else if (password) payload.password = password;

      await api.odoo.agentConfigSet(payload);
      setSaved(true);
      setTimeout(() => setSaved(false), 3000);
      // Reload to reflect the new masked/set state
      const fresh = await api.odoo.agentConfigGet(selected);
      applyCfg(fresh);
    } catch (e) {
      const detail = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
      setError(detail ? `${t('odoo.saveFailed')}: ${detail}` : t('odoo.saveFailed'));
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    if (!selected) return;
    setTesting(true);
    setTestResult(null);
    try {
      const res = await api.odoo.agentTest(selected);
      setTestResult({ ok: res.success, message: res.message });
    } catch (e) {
      const detail = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
      setTestResult({ ok: false, message: detail ? `${t('odoo.testFailed')}: ${detail}` : t('odoo.testFailed') });
    } finally {
      setTesting(false);
    }
  };

  const agentOptions: SelectOption[] = agents.map((a) => ({
    value: a.name,
    label: a.display_name || a.name,
  }));

  return (
    <SettingsSection title={t('odoo.agent.title')} description={t('odoo.agent.desc')}>
      {agents.length === 0 ? (
        <p className="py-6 text-center text-sm text-muted-foreground">{t('common.noData')}</p>
      ) : (
        <>
          <SettingsCard>
            <RowSelect
              label={t('odoo.agent.select')}
              value={selected}
              onChange={setSelected}
              options={agentOptions}
            />
          </SettingsCard>

          {!loading && cfg && (
            <Badge variant={cfg.configured ? 'default' : 'secondary'}>
              {cfg.configured ? t('odoo.agent.overridden') : t('odoo.agent.inherited')}
            </Badge>
          )}

          {loading ? (
            <div className="flex items-center justify-center py-10">
              <Spinner className="size-5 text-muted-foreground" />
            </div>
          ) : (
            <div className="space-y-4">
              <SettingsCard>
                <RowText label={t('agents.odoo.profile')} value={profile} onChange={setProfile} placeholder="default" tier="text" />
                <RowText label={t('odoo.url')} value={url} onChange={setUrl} placeholder="https://erp.example.com" tier="text" />
                <RowText label={t('odoo.db')} value={db} onChange={setDb} tier="text" />
                <RowText label={t('odoo.username')} value={username} onChange={setUsername} tier="text" />
              </SettingsCard>

              {/* Write-only credentials: blank keeps stored, clear-toggle wipes. */}
              <div className="grid gap-4 sm:grid-cols-2">
                <FieldBlock
                  label={t('odoo.apiKey')}
                  description={cfg?.api_key_set ? t('odoo.agent.secretSet') : t('odoo.agent.secretHint')}
                >
                  <Input
                    type="password"
                    autoComplete="off"
                    placeholder={cfg?.api_key_set ? '••••••••' : ''}
                    value={apiKey}
                    disabled={clearApiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                  />
                  {cfg?.api_key_set && (
                    <label className="flex items-center gap-2 text-xs text-muted-foreground">
                      <Checkbox
                        checked={clearApiKey}
                        onCheckedChange={(v) => setClearApiKey(Boolean(v))}
                        aria-label={t('odoo.agent.clearSecret')}
                      />
                      {t('odoo.agent.clearSecret')}
                    </label>
                  )}
                </FieldBlock>
                <FieldBlock
                  label={t('odoo.password')}
                  description={cfg?.password_set ? t('odoo.agent.secretSet') : t('odoo.agent.secretHint')}
                >
                  <Input
                    type="password"
                    autoComplete="off"
                    placeholder={cfg?.password_set ? '••••••••' : ''}
                    value={password}
                    disabled={clearPassword}
                    onChange={(e) => setPassword(e.target.value)}
                  />
                  {cfg?.password_set && (
                    <label className="flex items-center gap-2 text-xs text-muted-foreground">
                      <Checkbox
                        checked={clearPassword}
                        onCheckedChange={(v) => setClearPassword(Boolean(v))}
                        aria-label={t('odoo.agent.clearSecret')}
                      />
                      {t('odoo.agent.clearSecret')}
                    </label>
                  )}
                </FieldBlock>
              </div>

              <SettingsCard>
                <RowText
                  label={t('agents.odoo.allowedModels')}
                  description={t('agents.odoo.allowedModels.hint')}
                  value={allowedModels}
                  onChange={setAllowedModels}
                  placeholder="crm.lead, sale.order"
                  tier="text"
                />
                {pickedModels.length > 0 && (
                  <div className="flex flex-wrap items-center gap-2 px-1 pb-1">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        const current = parseList(allowedModels);
                        const merged = Array.from(new Set([...current, ...pickedModels]));
                        setAllowedModels(merged.join(', '));
                      }}
                    >
                      <Database className="size-4" />
                      {t('odoo.schema.addPicked').replace('{n}', String(pickedModels.length))}
                    </Button>
                  </div>
                )}
                <RowText
                  label={t('agents.odoo.unblockModels')}
                  description={t('agents.odoo.unblockModels.hint')}
                  value={unblockModels}
                  onChange={setUnblockModels}
                  placeholder="res.partner"
                  tier="text"
                />
                <RowText
                  label={t('agents.odoo.allowedActions')}
                  description={t('agents.odoo.allowedActions.hint')}
                  value={allowedActions}
                  onChange={setAllowedActions}
                  placeholder="read, write:crm.lead"
                  tier="text"
                />
                <RowText
                  label={t('agents.odoo.companyIds')}
                  description={t('agents.odoo.companyIds.hint')}
                  value={companyIds}
                  onChange={setCompanyIds}
                  placeholder="1, 2"
                  tier="text"
                />
              </SettingsCard>

              <div className="flex flex-wrap items-center justify-between gap-3 border-t border-surface-border pt-4">
                <div className="flex items-center gap-3">
                  <Button variant="outline" size="sm" onClick={handleTest} disabled={testing || saving}>
                    {testing ? <Loader2 className="animate-spin" /> : <RefreshCw />}
                    {t('odoo.agent.test')}
                  </Button>
                  {testResult && (
                    <span
                      className={cn(
                        'inline-flex items-center gap-1.5 text-sm font-medium',
                        testResult.ok ? 'text-success' : 'text-destructive'
                      )}
                    >
                      {testResult.ok ? <CheckCircle className="size-4" /> : <AlertTriangle className="size-4" />}
                      {testResult.message}
                    </span>
                  )}
                </div>
                <div className="flex items-center gap-3">
                  {error && <span className="text-sm text-destructive">{error}</span>}
                  {saved && (
                    <span className="inline-flex items-center gap-1.5 text-sm font-medium text-success">
                      <CheckCircle className="size-4" />
                      {t('common.saved')}
                    </span>
                  )}
                  <Button variant="brand" size="sm" onClick={handleSave} disabled={saving}>
                    {saving ? <Loader2 className="animate-spin" /> : <Save />}
                    {saving ? t('common.saving') : t('odoo.agent.save')}
                  </Button>
                </div>
              </div>
            </div>
          )}
        </>
      )}
    </SettingsSection>
  );
}
