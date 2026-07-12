import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import {
  api,
  type IdentityConfig,
  type IdentityProviderKind,
  type IdentityProjectsKind,
  type IdentityResolveResult,
} from '@/lib/api';
import {
  UserSearch,
  Loader2,
  Save,
  Search,
  CheckCircle,
  XCircle,
  ShieldCheck,
  ShieldAlert,
  Database,
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

/** Channels the test-resolve box can look up an identifier under. Mirrors the
 *  `ChannelKind` wire names in `duduclaw-identity`. */
const RESOLVE_CHANNELS = [
  'email',
  'discord',
  'line',
  'telegram',
  'slack',
  'whatsapp',
  'feishu',
  'webchat',
] as const;

/**
 * IdentityPage — dashboard surface for RFC-21 §1 identity resolution. Lets the
 * operator pick which provider answers "who sent this message?", configure the
 * Notion People DB, and test-resolve an identifier live (the demo hook: type an
 * email, see the canonical person + whether they're a project member).
 */
export function IdentityPage() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  // Provider selection + Notion config
  const [provider, setProvider] = useState<IdentityProviderKind>('wiki_cache');
  const [databaseId, setDatabaseId] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [apiKeySet, setApiKeySet] = useState(false);
  const [refreshSeconds, setRefreshSeconds] = useState('900');
  const [fmName, setFmName] = useState('Name');
  const [fmRoles, setFmRoles] = useState('Roles');
  const [fmProjects, setFmProjects] = useState('Projects');
  const [fmEmails, setFmEmails] = useState('Email');
  const [fmProjectsKind, setFmProjectsKind] = useState<IdentityProjectsKind>('multi_select');
  const [peopleDir, setPeopleDir] = useState('');

  // UI state
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Test-resolve state
  const [resolveInput, setResolveInput] = useState('');
  const [resolveChannel, setResolveChannel] = useState<string>('email');
  const [resolving, setResolving] = useState(false);
  const [resolveResult, setResolveResult] = useState<IdentityResolveResult | null>(null);
  const [resolveError, setResolveError] = useState<string | null>(null);

  const applyConfig = useCallback((cfg: IdentityConfig) => {
    setProvider(cfg.provider ?? 'wiki_cache');
    setDatabaseId(cfg.notion?.database_id ?? '');
    setApiKeySet(cfg.notion?.api_key_set ?? false);
    setRefreshSeconds(String(cfg.notion?.refresh_seconds ?? 900));
    const fm = cfg.notion?.field_map ?? {};
    setFmName(fm.name ?? 'Name');
    setFmRoles(fm.roles ?? 'Roles');
    setFmProjects(fm.projects ?? 'Projects');
    setFmEmails(fm.emails ?? 'Email');
    setFmProjectsKind(fm.projects_kind ?? 'multi_select');
    setPeopleDir(cfg.wiki_cache?.people_dir ?? '');
  }, []);

  const loadConfig = useCallback(async () => {
    setLoading(true);
    try {
      const cfg = await api.identity.configGet();
      applyConfig(cfg);
    } catch {
      setError(t('common.error'));
    } finally {
      setLoading(false);
    }
  }, [applyConfig, intl]);

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
      await api.identity.configSet({
        provider,
        notion: {
          database_id: databaseId.trim(),
          refresh_seconds: Math.max(0, Number(refreshSeconds) || 900),
          // Only send api_key when the operator typed a new one — an empty field
          // means "leave the stored secret untouched" (undefined, not '').
          ...(apiKey ? { api_key: apiKey } : {}),
          field_map: {
            name: fmName.trim() || 'Name',
            roles: fmRoles.trim() || 'Roles',
            projects: fmProjects.trim() || 'Projects',
            emails: fmEmails.trim() || 'Email',
            projects_kind: fmProjectsKind,
          },
        },
      });
      setSaved(true);
      if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
      savedTimerRef.current = setTimeout(() => setSaved(false), 3000);
      setApiKey('');
      await loadConfig();
    } catch (e) {
      const detail = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
      setError(detail ? `${t('identity.saveFailed')}: ${detail}` : t('identity.saveFailed'));
    } finally {
      setSaving(false);
    }
  };

  const handleResolve = async () => {
    if (!resolveInput.trim()) return;
    setResolving(true);
    setResolveResult(null);
    setResolveError(null);
    try {
      const res = await api.identity.resolve(resolveInput.trim(), resolveChannel);
      setResolveResult(res);
    } catch (e) {
      const detail = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
      setResolveError(detail ? `${t('identity.resolveFailed')}: ${detail}` : t('identity.resolveFailed'));
    } finally {
      setResolving(false);
    }
  };

  const showNotion = provider === 'notion' || provider === 'chained';

  if (loading) {
    return (
      <div className="flex items-center justify-center py-20">
        <Loader2 className="h-6 w-6 animate-spin text-stone-400" />
      </div>
    );
  }

  return (
    <Page>
      <PageHeader icon={UserSearch} title={t('identity.title')} subtitle={t('identity.subtitle')} />

      {/* Provider selection */}
      <Card title={t('identity.providerTitle')}>
        <p className="mb-4 text-sm text-stone-500 dark:text-stone-400">
          {t('identity.providerDesc')}
        </p>
        <Field label={t('identity.provider')} htmlFor="identity-provider">
          <select
            id="identity-provider"
            className={controlClass}
            value={provider}
            onChange={(e) => setProvider(e.target.value as IdentityProviderKind)}
          >
            <option value="wiki_cache">{t('identity.provider.wiki_cache')}</option>
            <option value="notion">{t('identity.provider.notion')}</option>
            <option value="chained">{t('identity.provider.chained')}</option>
          </select>
        </Field>
        <p className="mt-2 text-xs text-stone-500 dark:text-stone-400">
          {t(`identity.provider.${provider}.hint`)}
        </p>
        {peopleDir && (
          <p className="mt-3 inline-flex items-center gap-1.5 text-xs text-stone-500 dark:text-stone-400">
            <Database className="h-3.5 w-3.5" />
            {t('identity.peopleDir')}: <code className="font-mono">{peopleDir}</code>
          </p>
        )}
      </Card>

      {/* Notion settings — only when Notion is in the chain */}
      {showNotion && (
        <Card title={t('identity.notionTitle')}>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label={t('identity.databaseId')} htmlFor="identity-db" help={t('identity.databaseIdHint')}>
              <input
                id="identity-db"
                type="text"
                className={controlClass}
                placeholder="2f9c1e8a…"
                value={databaseId}
                onChange={(e) => setDatabaseId(e.target.value)}
              />
            </Field>

            <Field
              label={t('identity.apiKey')}
              htmlFor="identity-api-key"
              help={apiKeySet ? t('identity.apiKeySetHint') : t('identity.apiKeyHint')}
            >
              <input
                id="identity-api-key"
                type="password"
                className={controlClass}
                placeholder={apiKeySet ? '••••••••' : 'secret_…'}
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
              />
            </Field>

            <Field label={t('identity.refreshSeconds')} htmlFor="identity-refresh">
              <input
                id="identity-refresh"
                type="number"
                className={controlClass}
                min={0}
                value={refreshSeconds}
                onChange={(e) => setRefreshSeconds(e.target.value)}
              />
            </Field>
          </div>

          <Section className="mt-5 space-y-3">
            <p className="text-sm font-medium text-stone-700 dark:text-stone-300">
              {t('identity.fieldMap')}
            </p>
            <p className="text-xs text-stone-500 dark:text-stone-400">{t('identity.fieldMapHint')}</p>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label={t('identity.fm.name')} htmlFor="identity-fm-name">
                <input id="identity-fm-name" type="text" className={controlClass} value={fmName} onChange={(e) => setFmName(e.target.value)} />
              </Field>
              <Field label={t('identity.fm.roles')} htmlFor="identity-fm-roles">
                <input id="identity-fm-roles" type="text" className={controlClass} value={fmRoles} onChange={(e) => setFmRoles(e.target.value)} />
              </Field>
              <Field label={t('identity.fm.projects')} htmlFor="identity-fm-projects">
                <input id="identity-fm-projects" type="text" className={controlClass} value={fmProjects} onChange={(e) => setFmProjects(e.target.value)} />
              </Field>
              <Field label={t('identity.fm.emails')} htmlFor="identity-fm-emails">
                <input id="identity-fm-emails" type="text" className={controlClass} value={fmEmails} onChange={(e) => setFmEmails(e.target.value)} />
              </Field>
              <Field label={t('identity.fm.projectsKind')} htmlFor="identity-fm-projects-kind">
                <select
                  id="identity-fm-projects-kind"
                  className={controlClass}
                  value={fmProjectsKind}
                  onChange={(e) => setFmProjectsKind(e.target.value as IdentityProjectsKind)}
                >
                  <option value="multi_select">{t('identity.projectsKind.multi_select')}</option>
                  <option value="relation">{t('identity.projectsKind.relation')}</option>
                </select>
              </Field>
            </div>
          </Section>
        </Card>
      )}

      {/* Save bar */}
      <div className="flex items-center justify-end gap-3">
        {error && <span className="text-sm text-rose-600 dark:text-rose-400">{error}</span>}
        {saved && (
          <span className="inline-flex items-center gap-1.5 text-sm font-medium text-emerald-600">
            <CheckCircle className="h-4 w-4" />
            {t('common.saved')}
          </span>
        )}
        <Button variant="primary" onClick={handleSave} disabled={saving} icon={saving ? undefined : Save}>
          {saving && <Loader2 className="h-4 w-4 animate-spin" />}
          {saving ? t('common.saving') : t('common.save')}
        </Button>
      </div>

      {/* Test resolve — the demo box */}
      <Card
        title={
          <span className="flex items-center gap-2">
            <Search className="h-4 w-4 text-stone-400" />
            {t('identity.testTitle')}
          </span>
        }
      >
        <p className="mb-4 text-sm text-stone-500 dark:text-stone-400">{t('identity.testDesc')}</p>
        <div className="flex flex-col gap-3 sm:flex-row sm:items-end">
          <Field label={t('identity.testChannel')} htmlFor="identity-test-channel" className="sm:w-44">
            <select
              id="identity-test-channel"
              className={controlClass}
              value={resolveChannel}
              onChange={(e) => setResolveChannel(e.target.value)}
            >
              {RESOLVE_CHANNELS.map((ch) => (
                <option key={ch} value={ch}>
                  {t(`identity.channel.${ch}`)}
                </option>
              ))}
            </select>
          </Field>
          <Field label={t('identity.testInput')} htmlFor="identity-test-input" className="flex-1">
            <input
              id="identity-test-input"
              type="text"
              className={controlClass}
              placeholder={t('identity.testInputPlaceholder')}
              value={resolveInput}
              onChange={(e) => setResolveInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleResolve();
              }}
            />
          </Field>
          <Button
            variant="secondary"
            onClick={handleResolve}
            disabled={resolving || !resolveInput.trim()}
            icon={resolving ? undefined : Search}
          >
            {resolving && <Loader2 className="h-4 w-4 animate-spin" />}
            {t('identity.testButton')}
          </Button>
        </div>

        {resolveError && (
          <p className="mt-4 text-sm text-rose-600 dark:text-rose-400">{resolveError}</p>
        )}

        {resolveResult && !resolveError && (
          <div className="mt-5">
            {resolveResult.found && resolveResult.person ? (
              <div className="rounded-lg border border-[var(--panel-border)] bg-[var(--panel-fill)] p-4">
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0">
                    <p className="text-base font-semibold text-stone-900 dark:text-stone-50">
                      {resolveResult.person.display_name}
                    </p>
                    <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">
                      {t('identity.result.via')} {resolveResult.provider}
                    </p>
                  </div>
                  {resolveResult.is_project_member ? (
                    <Badge tone="success" dot>
                      <ShieldCheck className="h-3.5 w-3.5" />
                      {t('identity.result.member')}
                    </Badge>
                  ) : (
                    <Badge tone="warning" dot>
                      <ShieldAlert className="h-3.5 w-3.5" />
                      {t('identity.result.stranger')}
                    </Badge>
                  )}
                </div>

                <dl className="mt-4 grid gap-3 sm:grid-cols-2">
                  {resolveResult.person.roles.length > 0 && (
                    <div>
                      <dt className="text-xs text-stone-500 dark:text-stone-400">{t('identity.result.roles')}</dt>
                      <dd className="mt-1 flex flex-wrap gap-1">
                        {resolveResult.person.roles.map((r) => (
                          <Badge key={r} tone="neutral">{r}</Badge>
                        ))}
                      </dd>
                    </div>
                  )}
                  {resolveResult.person.project_ids.length > 0 && (
                    <div>
                      <dt className="text-xs text-stone-500 dark:text-stone-400">{t('identity.result.projects')}</dt>
                      <dd className="mt-1 flex flex-wrap gap-1">
                        {resolveResult.person.project_ids.map((p) => (
                          <Badge key={p} tone="neutral">{p}</Badge>
                        ))}
                      </dd>
                    </div>
                  )}
                  {resolveResult.person.emails.length > 0 && (
                    <div>
                      <dt className="text-xs text-stone-500 dark:text-stone-400">{t('identity.result.emails')}</dt>
                      <dd className="mt-1 text-sm text-stone-700 dark:text-stone-300">
                        {resolveResult.person.emails.join(', ')}
                      </dd>
                    </div>
                  )}
                </dl>
              </div>
            ) : (
              <div className="inline-flex items-center gap-2 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-800 dark:bg-amber-900/20 dark:text-amber-200">
                <XCircle className="h-4 w-4" />
                {t('identity.result.notFound')}
              </div>
            )}
          </div>
        )}
      </Card>
    </Page>
  );
}
