import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type OdooStatus } from '@/lib/api';
import {
  Building2,
  Briefcase,
  CheckCircle,
  XCircle,
  Loader2,
  RefreshCw,
  Save,
  ShoppingCart,
  Package,
  Calculator,
  FolderKanban,
  Users,
  Plug,
  AlertTriangle,
} from 'lucide-react';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Button,
  Badge,
  Field,
  controlClass,
} from '@/components/ui';

const FEATURE_MODULES = [
  { key: 'crm', icon: Users, color: 'text-blue-500' },
  { key: 'sale', icon: ShoppingCart, color: 'text-emerald-500' },
  { key: 'inventory', icon: Package, color: 'text-amber-500' },
  { key: 'accounting', icon: Calculator, color: 'text-violet-500' },
  { key: 'project', icon: FolderKanban, color: 'text-rose-500' },
  { key: 'hr', icon: Briefcase, color: 'text-cyan-500' },
] as const;

type FeatureKey = (typeof FEATURE_MODULES)[number]['key'];

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

  const toggleFeature = (key: FeatureKey) => {
    setFeatures((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-20">
        <Loader2 className="h-6 w-6 animate-spin text-stone-400" />
      </div>
    );
  }

  return (
    <Page>
      <PageHeader
        icon={Building2}
        title={t('nav.odoo')}
        subtitle={t('odoo.subtitle')}
        actions={
          status ? (
            <div className="flex flex-col items-end gap-1">
              <Badge tone={status.connected ? 'success' : 'neutral'} dot>
                {status.connected ? (
                  <CheckCircle className="h-3.5 w-3.5" />
                ) : (
                  <XCircle className="h-3.5 w-3.5" />
                )}
                {status.connected ? t('odoo.connected') : t('odoo.disconnected')}
                {status.edition && (
                  <span className="ml-0.5 opacity-70">({status.edition})</span>
                )}
              </Badge>
              {!status.connected && status.error && (
                <p className="text-xs text-rose-500 dark:text-rose-400">{status.error}</p>
              )}
            </div>
          ) : undefined
        }
      />

      {/* Connection Settings */}
      <Card
        title={
          <span className="flex items-center gap-2">
            <Plug className="h-4 w-4 text-stone-400" />
            {t('odoo.connection')}
          </span>
        }
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label={t('odoo.url')} htmlFor="odoo-url">
            <input
              id="odoo-url"
              type="url"
              className={controlClass}
              placeholder="https://mycompany.odoo.com"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
          </Field>

          <Field label={t('odoo.db')} htmlFor="odoo-db">
            <input
              id="odoo-db"
              type="text"
              className={controlClass}
              placeholder="mycompany"
              value={db}
              onChange={(e) => setDb(e.target.value)}
            />
          </Field>

          <Field label={t('odoo.protocol')} htmlFor="odoo-protocol">
            <select
              id="odoo-protocol"
              className={controlClass}
              value={protocol}
              onChange={(e) => setProtocol(e.target.value)}
            >
              <option value="jsonrpc">JSON-RPC</option>
              <option value="xmlrpc">XML-RPC</option>
            </select>
          </Field>

          <Field label={t('odoo.authMethod')} htmlFor="odoo-auth-method">
            <select
              id="odoo-auth-method"
              className={controlClass}
              value={authMethod}
              onChange={(e) => setAuthMethod(e.target.value)}
            >
              <option value="api_key">{t('odoo.authApiKey')}</option>
              <option value="password">{t('odoo.authPassword')}</option>
            </select>
          </Field>

          <Field label={t('odoo.username')} htmlFor="odoo-username">
            <input
              id="odoo-username"
              type="text"
              className={controlClass}
              placeholder="admin@mycompany.com"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
            />
          </Field>

          {authMethod === 'api_key' ? (
            <Field label={t('odoo.apiKey')} htmlFor="odoo-api-key" help={t('odoo.apiKeyHint')}>
              <input
                id="odoo-api-key"
                type="password"
                className={controlClass}
                placeholder="••••••••"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
              />
            </Field>
          ) : (
            <Field label={t('odoo.password')} htmlFor="odoo-password">
              <input
                id="odoo-password"
                type="password"
                className={controlClass}
                placeholder="••••••••"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
          )}
        </div>

        {/* Test connection */}
        <div className="mt-5 flex items-center gap-3">
          <Button
            variant="secondary"
            onClick={handleTest}
            disabled={testing || !url.trim() || !db.trim() || saving}
            title={!status?.connected ? t('odoo.testHint') : undefined}
            icon={testing ? undefined : RefreshCw}
          >
            {testing && <Loader2 className="h-4 w-4 animate-spin" />}
            {t('odoo.testConnection')}
          </Button>

          {testResult && (
            <span
              className={cn(
                'inline-flex items-center gap-1.5 text-sm font-medium',
                testResult.ok ? 'text-emerald-600' : 'text-rose-600'
              )}
            >
              {testResult.ok ? (
                <CheckCircle className="h-4 w-4" />
              ) : (
                <AlertTriangle className="h-4 w-4" />
              )}
              {testResult.message}
            </span>
          )}
        </div>
      </Card>

      {/* Feature Modules */}
      <Card title={t('odoo.features')}>
        <p className="mb-5 text-sm text-stone-500 dark:text-stone-400">
          {t('odoo.featuresDesc')}
        </p>

        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {FEATURE_MODULES.map(({ key, icon: Icon, color }) => (
            <button
              key={key}
              role="switch"
              aria-checked={features[key]}
              onClick={() => toggleFeature(key)}
              className={cn(
                'flex items-center gap-3 rounded-lg border px-4 py-3 text-left transition-colors',
                features[key]
                  ? 'border-amber-300 bg-amber-50 dark:border-amber-700 dark:bg-amber-900/20'
                  : 'border-[var(--panel-border)] bg-[var(--panel-fill)] hover:bg-[var(--panel-fill-hover)]'
              )}
            >
              <Icon className={cn('h-5 w-5 shrink-0', features[key] ? color : 'text-stone-400')} />
              <div className="min-w-0 flex-1">
                <p className="text-sm font-medium text-stone-900 dark:text-stone-50">
                  {t(`odoo.feature.${key}`)}
                </p>
                <p className="text-xs text-stone-500 dark:text-stone-400">
                  {t(`odoo.feature.${key}.desc`)}
                </p>
              </div>
              <div
                className={cn(
                  'h-5 w-9 shrink-0 rounded-full transition-colors',
                  features[key] ? 'bg-amber-500' : 'bg-stone-300 dark:bg-stone-600'
                )}
              >
                <div
                  className={cn(
                    'h-5 w-5 rounded-full bg-white shadow-sm transition-transform',
                    features[key] ? 'translate-x-4' : 'translate-x-0'
                  )}
                />
              </div>
            </button>
          ))}
        </div>
      </Card>

      {/* Polling & Webhook */}
      <Card title={t('odoo.sync')}>
        <div className="grid gap-5 sm:grid-cols-2">
          {/* Polling */}
          <Section className="space-y-3">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={pollEnabled}
                onChange={(e) => setPollEnabled(e.target.checked)}
                className="h-4 w-4 rounded border-stone-300 text-amber-500 focus:ring-amber-500"
              />
              <span className="text-sm font-medium text-stone-700 dark:text-stone-300">
                {t('odoo.pollEnabled')}
              </span>
            </label>

            {pollEnabled && (
              <>
                <Field label={t('odoo.pollInterval')} htmlFor="odoo-poll-interval" help={t('odoo.pollIntervalHint')}>
                  <input
                    id="odoo-poll-interval"
                    type="number"
                    className={controlClass}
                    min={60}
                    max={86400}
                    value={pollInterval}
                    onChange={(e) => setPollInterval(e.target.value)}
                  />
                </Field>

                <Field label={t('odoo.pollModels')} htmlFor="odoo-poll-models" help={t('odoo.pollModelsHint')}>
                  <input
                    id="odoo-poll-models"
                    type="text"
                    className={controlClass}
                    placeholder="crm.lead,sale.order"
                    value={pollModels}
                    onChange={(e) => setPollModels(e.target.value)}
                  />
                </Field>
              </>
            )}
          </Section>

          {/* Webhook */}
          <Section className="space-y-3">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={webhookEnabled}
                onChange={(e) => setWebhookEnabled(e.target.checked)}
                className="h-4 w-4 rounded border-stone-300 text-amber-500 focus:ring-amber-500"
              />
              <span className="text-sm font-medium text-stone-700 dark:text-stone-300">
                {t('odoo.webhookEnabled')}
              </span>
            </label>

            {webhookEnabled && (
              <Field label={t('odoo.webhookSecret')} htmlFor="odoo-webhook-secret" help={t('odoo.webhookSecretHint')}>
                <input
                  id="odoo-webhook-secret"
                  type="password"
                  className={controlClass}
                  placeholder="••••••••"
                  value={webhookSecret}
                  onChange={(e) => setWebhookSecret(e.target.value)}
                />
              </Field>
            )}
          </Section>
        </div>
      </Card>

      {/* Save bar */}
      <div className="flex items-center justify-end gap-3">
        {error && (
          <span className="text-sm text-rose-600 dark:text-rose-400">{error}</span>
        )}
        {saved && (
          <span className="inline-flex items-center gap-1.5 text-sm font-medium text-emerald-600">
            <CheckCircle className="h-4 w-4" />
            {t('common.saved')}
          </span>
        )}
        <Button
          variant="primary"
          onClick={handleSave}
          disabled={saving || !url.trim()}
          icon={saving ? undefined : Save}
        >
          {saving && <Loader2 className="h-4 w-4 animate-spin" />}
          {saving ? t('common.saving') : t('common.save')}
        </Button>
      </div>
    </Page>
  );
}
