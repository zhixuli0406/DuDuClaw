import { useEffect, useState, useCallback, useRef, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import {
  api,
  type IdentityConfig,
  type IdentityProviderKind,
  type IdentityProjectsKind,
  type IdentityResolveResult,
} from '@/lib/api';
import {
  Loader2,
  Save,
  Search,
  CheckCircle,
  XCircle,
  ShieldCheck,
  ShieldAlert,
  Database,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { isImeComposing } from '@/lib/keyboard';
import {
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  Button,
  Badge,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/mds';

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

const PROVIDER_KINDS: readonly IdentityProviderKind[] = ['wiki_cache', 'notion', 'chained'];
const PROJECTS_KINDS: readonly IdentityProjectsKind[] = ['multi_select', 'relation'];

/** Local labeled-field wrapper (spec §4 form pattern) — label over control. */
function Field({
  label,
  htmlFor,
  help,
  className,
  children,
}: {
  label: string;
  htmlFor?: string;
  help?: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={cn('space-y-1.5', className)}>
      <label htmlFor={htmlFor} className="text-xs font-medium text-muted-foreground">
        {label}
      </label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

/**
 * IdentityPage — dashboard surface for RFC-21 §1 identity resolution (MDS,
 * embedded as the "identity" tab of `/manage/integrations`). Lets the operator
 * pick which provider answers "who sent this message?", configure the Notion
 * People DB, and test-resolve an identifier live.
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
        <Loader2 className="size-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Provider selection */}
      <Card>
        <CardHeader>
          <CardTitle>{t('identity.providerTitle')}</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <p className="text-sm text-muted-foreground">{t('identity.providerDesc')}</p>
          <Field label={t('identity.provider')} htmlFor="identity-provider">
            <Select value={provider} onValueChange={(v) => setProvider(String(v) as IdentityProviderKind)}>
              <SelectTrigger id="identity-provider" className="w-full">
                <SelectValue>{t(`identity.provider.${provider}`)}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {PROVIDER_KINDS.map((k) => (
                  <SelectItem key={k} value={k}>
                    {t(`identity.provider.${k}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          <p className="text-xs text-muted-foreground">{t(`identity.provider.${provider}.hint`)}</p>
          {peopleDir && (
            <p className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
              <Database className="size-3.5" />
              {t('identity.peopleDir')}: <code className="font-mono">{peopleDir}</code>
            </p>
          )}
        </CardContent>
      </Card>

      {/* Notion settings — only when Notion is in the chain */}
      {showNotion && (
        <Card>
          <CardHeader>
            <CardTitle>{t('identity.notionTitle')}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-5">
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label={t('identity.databaseId')} htmlFor="identity-db" help={t('identity.databaseIdHint')}>
                <Input
                  id="identity-db"
                  type="text"
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
                <Input
                  id="identity-api-key"
                  type="password"
                  placeholder={apiKeySet ? '••••••••' : 'secret_…'}
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                />
              </Field>

              <Field label={t('identity.refreshSeconds')} htmlFor="identity-refresh">
                <Input
                  id="identity-refresh"
                  type="number"
                  min={0}
                  value={refreshSeconds}
                  onChange={(e) => setRefreshSeconds(e.target.value)}
                />
              </Field>
            </div>

            <div className="space-y-3">
              <p className="text-sm font-medium text-foreground">{t('identity.fieldMap')}</p>
              <p className="text-xs text-muted-foreground">{t('identity.fieldMapHint')}</p>
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label={t('identity.fm.name')} htmlFor="identity-fm-name">
                  <Input id="identity-fm-name" type="text" value={fmName} onChange={(e) => setFmName(e.target.value)} />
                </Field>
                <Field label={t('identity.fm.roles')} htmlFor="identity-fm-roles">
                  <Input id="identity-fm-roles" type="text" value={fmRoles} onChange={(e) => setFmRoles(e.target.value)} />
                </Field>
                <Field label={t('identity.fm.projects')} htmlFor="identity-fm-projects">
                  <Input id="identity-fm-projects" type="text" value={fmProjects} onChange={(e) => setFmProjects(e.target.value)} />
                </Field>
                <Field label={t('identity.fm.emails')} htmlFor="identity-fm-emails">
                  <Input id="identity-fm-emails" type="text" value={fmEmails} onChange={(e) => setFmEmails(e.target.value)} />
                </Field>
                <Field label={t('identity.fm.projectsKind')} htmlFor="identity-fm-projects-kind">
                  <Select
                    value={fmProjectsKind}
                    onValueChange={(v) => setFmProjectsKind(String(v) as IdentityProjectsKind)}
                  >
                    <SelectTrigger id="identity-fm-projects-kind" className="w-full">
                      <SelectValue>{t(`identity.projectsKind.${fmProjectsKind}`)}</SelectValue>
                    </SelectTrigger>
                    <SelectContent>
                      {PROJECTS_KINDS.map((k) => (
                        <SelectItem key={k} value={k}>
                          {t(`identity.projectsKind.${k}`)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Save bar */}
      <div className="flex items-center justify-end gap-3">
        {error && <span className="text-sm text-destructive">{error}</span>}
        {saved && (
          <span className="inline-flex items-center gap-1.5 text-sm font-medium text-success">
            <CheckCircle className="size-4" />
            {t('common.saved')}
          </span>
        )}
        <Button variant="brand" onClick={handleSave} disabled={saving}>
          {saving ? <Loader2 className="size-4 animate-spin" /> : <Save />}
          {saving ? t('common.saving') : t('common.save')}
        </Button>
      </div>

      {/* Test resolve — the demo box */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Search className="size-4 text-muted-foreground" />
            {t('identity.testTitle')}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <p className="text-sm text-muted-foreground">{t('identity.testDesc')}</p>
          <div className="flex flex-col gap-3 sm:flex-row sm:items-end">
            <Field label={t('identity.testChannel')} htmlFor="identity-test-channel" className="sm:w-44">
              <Select value={resolveChannel} onValueChange={(v) => setResolveChannel(String(v))}>
                <SelectTrigger id="identity-test-channel" className="w-full">
                  <SelectValue>{t(`identity.channel.${resolveChannel}`)}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {RESOLVE_CHANNELS.map((ch) => (
                    <SelectItem key={ch} value={ch}>
                      {t(`identity.channel.${ch}`)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label={t('identity.testInput')} htmlFor="identity-test-input" className="flex-1">
              <Input
                id="identity-test-input"
                type="text"
                placeholder={t('identity.testInputPlaceholder')}
                value={resolveInput}
                onChange={(e) => setResolveInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !isImeComposing(e)) handleResolve();
                }}
              />
            </Field>
            <Button variant="outline" onClick={handleResolve} disabled={resolving || !resolveInput.trim()}>
              {resolving ? <Loader2 className="size-4 animate-spin" /> : <Search />}
              {t('identity.testButton')}
            </Button>
          </div>

          {resolveError && <p className="text-sm text-destructive">{resolveError}</p>}

          {resolveResult && !resolveError && (
            <div>
              {resolveResult.found && resolveResult.person ? (
                <div className="rounded-lg border border-surface-border bg-muted/50 p-4">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <p className="text-base font-medium text-foreground">
                        {resolveResult.person.display_name}
                      </p>
                      <p className="mt-0.5 text-xs text-muted-foreground">
                        {t('identity.result.via')} {resolveResult.provider}
                      </p>
                    </div>
                    {resolveResult.is_project_member ? (
                      <Badge variant="secondary" className="bg-success/15 text-success">
                        <ShieldCheck />
                        {t('identity.result.member')}
                      </Badge>
                    ) : (
                      <Badge variant="secondary" className="bg-warning/15 text-warning">
                        <ShieldAlert />
                        {t('identity.result.stranger')}
                      </Badge>
                    )}
                  </div>

                  <dl className="mt-4 grid gap-3 sm:grid-cols-2">
                    {resolveResult.person.roles.length > 0 && (
                      <div>
                        <dt className="text-xs text-muted-foreground">{t('identity.result.roles')}</dt>
                        <dd className="mt-1 flex flex-wrap gap-1">
                          {resolveResult.person.roles.map((r) => (
                            <Badge key={r} variant="secondary">{r}</Badge>
                          ))}
                        </dd>
                      </div>
                    )}
                    {resolveResult.person.project_ids.length > 0 && (
                      <div>
                        <dt className="text-xs text-muted-foreground">{t('identity.result.projects')}</dt>
                        <dd className="mt-1 flex flex-wrap gap-1">
                          {resolveResult.person.project_ids.map((p) => (
                            <Badge key={p} variant="secondary">{p}</Badge>
                          ))}
                        </dd>
                      </div>
                    )}
                    {resolveResult.person.emails.length > 0 && (
                      <div>
                        <dt className="text-xs text-muted-foreground">{t('identity.result.emails')}</dt>
                        <dd className="mt-1 text-sm text-foreground">
                          {resolveResult.person.emails.join(', ')}
                        </dd>
                      </div>
                    )}
                  </dl>
                </div>
              ) : (
                <div className="inline-flex items-center gap-2 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm text-warning">
                  <XCircle className="size-4" />
                  {t('identity.result.notFound')}
                </div>
              )}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
