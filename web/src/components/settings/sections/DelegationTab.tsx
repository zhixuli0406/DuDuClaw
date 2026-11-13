import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, ArrowLeftRight, Plus, Trash2 } from 'lucide-react';
import { api, type AgentDetail, type DelegationPolicy } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import {
  Badge,
  Button,
  ErrorState,
  SettingsCard,
  SettingsSection,
  SettingsSaveState,
} from '@/components/mds';
import { OptionSelect, type SelectOption } from '@/components/settings/controls';

export const DELEGATION_POLICIES: DelegationPolicy[] = ['department', 'hierarchy', 'open', 'none'];

/** A whitelist row while it is being edited — either side may still be blank. */
type PairDraft = { a: string; b: string };

/**
 * 委派權限 (WP21 §2.8) — who may hand work to whom, org-wide.
 *
 * Two controls: the delegation mode (four mutually exclusive options, `none`
 * since v1.82) and the cross-department pair list that opens specific two-way
 * exceptions on top of it. Both are written to the gateway in one
 * `delegation.set` call and take effect on the next delegation, with no
 * restart.
 *
 * RM-137: the form only ever submits what it loaded. Until `delegation.get`
 * has succeeded, every control and the save button are disabled — a failed
 * read used to leave the `department` + no-pairs defaults on screen, and one
 * click on save wrote them over the real setting.
 *
 * Owner/admin only. The gateway gates both RPCs as well — this component
 * rendering `null` for everyone else is the polite half, not the enforcement.
 */
export function DelegationTab() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');

  // Placeholders only: nothing below is shown as a setting or submitted
  // until `loaded` is true.
  const [policy, setPolicy] = useState<DelegationPolicy>('department');
  const [pairs, setPairs] = useState<PairDraft[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [agents, setAgents] = useState<AgentDetail[]>([]);
  const [loading, setLoading] = useState(true);
  // True only after the latest load succeeded.
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // P05: a failed read left the radio group on its `department` default and an
  // empty pair list — visually identical to a real, saved configuration.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    setLoadError(null);
    setLoaded(false);
    setLoading(true);
    try {
      const [settings, list] = await Promise.all([
        api.delegation.get(),
        api.agents.list(),
      ]);
      setPolicy(settings.policy);
      setPairs(settings.allow.map(([a, b]) => ({ a, b })));
      setWarnings(settings.warnings ?? []);
      setAgents(list.agents ?? []);
      setLoaded(true);
    } catch (e) {
      console.warn('[api]', e);
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    if (!isAdmin) {
      setLoading(false);
      return;
    }
    void load();
  }, [isAdmin, load]);

  if (!isAdmin) return null;

  const agentLabel = (name: string) =>
    agents.find((a) => a.name === name)?.display_name || name;
  const agentDepartment = (name: string) =>
    agents.find((a) => a.name === name)?.department || '';

  const agentOptions: SelectOption[] = [
    { value: '', label: t('settings.delegation.pair.pick') },
    ...agents.map((a) => ({
      value: a.name,
      label: a.department
        ? `${a.display_name || a.name}（${a.department}）`
        : (a.display_name || a.name),
    })),
  ];

  const incomplete = pairs.some((p) => !p.a || !p.b);
  const hasSelfPair = pairs.some((p) => p.a && p.a === p.b);
  // Editing is possible only on a successfully loaded setting.
  const editable = loaded && !loading && loadError == null;
  const canSave = editable && !saving && !incomplete && !hasSelfPair;

  const updatePair = (index: number, side: 'a' | 'b', value: string) => {
    setPairs((prev) => prev.map((p, i) => (i === index ? { ...p, [side]: value } : p)));
  };

  const handleSave = async () => {
    // Never submit a state that was not loaded (the button is disabled too).
    if (!canSave) return;
    setSaving(true);
    setSaved(false);
    setSaveError(null);
    try {
      await api.delegation.set({
        policy,
        allow: pairs.map((p) => [p.a, p.b] as [string, string]),
      });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
      // Re-read so any server-side cleaning (dedup, dropped self-pairs) is
      // what the operator ends up looking at.
      await load();
    } catch (e) {
      console.warn('[api]', e);
      setSaveError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-8">
      {loadError != null && (
        <ErrorState
          variant="inline"
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          onRetry={() => void load()}
        />
      )}

      {warnings.length > 0 && (
        <div className="rounded-lg border border-warning/30 bg-warning/10 p-3 text-xs text-foreground">
          <p className="mb-1 flex items-center gap-1.5 font-medium">
            <AlertTriangle className="size-3.5 text-warning" />
            {t('settings.delegation.warnings')}
          </p>
          <ul className="ml-5 list-disc space-y-0.5 text-muted-foreground">
            {warnings.map((w, i) => <li key={i}>{w}</li>)}
          </ul>
        </div>
      )}

      {/* ① Delegation mode */}
      <SettingsSection
        title={t('settings.delegation.mode')}
        description={t('settings.delegation.mode.desc')}
      >
        <SettingsCard>
          {DELEGATION_POLICIES.map((p) => (
            <label
              key={p}
              className="flex cursor-pointer items-start gap-3 px-4 py-3.5 transition-colors hover:bg-surface-hover"
            >
              <input
                type="radio"
                name="delegation-policy"
                value={p}
                className="mt-1 accent-primary"
                checked={loaded && policy === p}
                disabled={!editable}
                onChange={() => setPolicy(p)}
              />
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium text-foreground">
                  {t(`settings.delegation.policy.${p}`)}
                </span>
                <span className="mt-0.5 block text-xs text-muted-foreground">
                  {t(`settings.delegation.policy.${p}.help`)}
                </span>
              </span>
            </label>
          ))}
        </SettingsCard>
        {loaded && policy === 'open' && (
          <p className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-foreground">
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-warning" />
            {t('settings.delegation.policy.open.warning')}
          </p>
        )}
        {loaded && policy === 'none' && (
          <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
            {t('settings.delegation.policy.none.note')}
          </p>
        )}
      </SettingsSection>

      {/* ② Cross-department pairs */}
      <SettingsSection
        title={t('settings.delegation.pairs')}
        description={t('settings.delegation.pairs.desc')}
      >
        {loaded && policy === 'none' && pairs.length > 0 && (
          <p className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-foreground">
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-warning" />
            {t('settings.delegation.pairs.inactiveUnderNone')}
          </p>
        )}
        <SettingsCard>
          {loaded && pairs.length === 0 && (
            <p className="px-4 py-4 text-center text-xs text-muted-foreground">
              {t('settings.delegation.pairs.empty')}
            </p>
          )}
          {!loaded && (
            <p className="px-4 py-4 text-center text-xs text-muted-foreground">
              {loading ? t('common.loading') : t('settings.delegation.notLoaded')}
            </p>
          )}
          {loaded && pairs.map((pair, index) => (
            <div
              key={index}
              className="flex flex-wrap items-center gap-2 px-4 py-3 sm:flex-nowrap"
            >
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <OptionSelect
                  value={pair.a}
                  showRaw={false}
                  options={agentOptions}
                  disabled={!editable}
                  onChange={(v) => updatePair(index, 'a', v)}
                />
                {agentDepartment(pair.a) && (
                  <Badge variant="secondary" className="shrink-0">{agentDepartment(pair.a)}</Badge>
                )}
              </div>
              <ArrowLeftRight className="size-4 shrink-0 text-muted-foreground" />
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <OptionSelect
                  value={pair.b}
                  showRaw={false}
                  options={agentOptions}
                  disabled={!editable}
                  onChange={(v) => updatePair(index, 'b', v)}
                />
                {agentDepartment(pair.b) && (
                  <Badge variant="secondary" className="shrink-0">{agentDepartment(pair.b)}</Badge>
                )}
              </div>
              <Button
                variant="ghost"
                size="sm"
                disabled={!editable}
                aria-label={intl.formatMessage(
                  { id: 'settings.delegation.pair.remove' },
                  { a: agentLabel(pair.a), b: agentLabel(pair.b) },
                )}
                onClick={() => setPairs((prev) => prev.filter((_, i) => i !== index))}
              >
                <Trash2 className="size-4" />
              </Button>
            </div>
          ))}
          <div className="px-4 py-3">
            <Button
              variant="outline"
              size="sm"
              disabled={!editable || agents.length < 2}
              onClick={() => setPairs((prev) => [...prev, { a: '', b: '' }])}
            >
              <Plus className="size-4" />
              {t('settings.delegation.pairs.add')}
            </Button>
          </div>
        </SettingsCard>
        <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
          {t('settings.delegation.pairs.help')}
        </p>
      </SettingsSection>

      {saveError != null && (
        <ErrorState
          variant="inline"
          error={saveError}
          title={intl.formatMessage({ id: 'errorState.manage.saveFailed' })}
          description={intl.formatMessage({ id: 'errorState.manage.saveFailedHint' })}
          onRetry={() => void handleSave()}
        />
      )}
      <div className="flex flex-wrap items-center justify-end gap-3">
        {!loaded && !loading && (
          <span className="text-xs text-warning">{t('settings.delegation.saveBlocked')}</span>
        )}
        {incomplete && (
          <span className="text-xs text-warning">{t('settings.delegation.pair.incomplete')}</span>
        )}
        {!incomplete && hasSelfPair && (
          <span className="text-xs text-warning">{t('settings.delegation.pair.same')}</span>
        )}
        <span className="mr-auto text-xs text-muted-foreground">
          {t('settings.delegation.applied')}
        </span>
        <SettingsSaveState
          status={saving ? 'saving' : saved ? 'saved' : 'idle'}
          savingLabel={intl.formatMessage({ id: 'common.saving' })}
          savedLabel={intl.formatMessage({ id: 'settings.general.saved' })}
        />
        <Button variant="brand" size="sm" onClick={handleSave} disabled={!canSave}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </div>
  );
}
