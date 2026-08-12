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

const POLICIES: DelegationPolicy[] = ['department', 'hierarchy', 'open'];

/** A whitelist row while it is being edited — either side may still be blank. */
type PairDraft = { a: string; b: string };

/**
 * 委派權限 (WP21 §2.8) — who may hand work to whom, org-wide.
 *
 * Two controls: the delegation mode (three mutually exclusive options) and the
 * cross-department pair list that opens specific two-way exceptions on top of
 * it. Both are written to the gateway in one `delegation.set` call and take
 * effect on the next delegation, with no restart.
 *
 * Owner/admin only. The gateway gates both RPCs as well — this component
 * rendering `null` for everyone else is the polite half, not the enforcement.
 */
export function DelegationTab() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');

  const [policy, setPolicy] = useState<DelegationPolicy>('department');
  const [pairs, setPairs] = useState<PairDraft[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [agents, setAgents] = useState<AgentDetail[]>([]);
  const [loading, setLoading] = useState(true);
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
    try {
      const [settings, list] = await Promise.all([
        api.delegation.get(),
        api.agents.list(),
      ]);
      setPolicy(settings.policy);
      setPairs(settings.allow.map(([a, b]) => ({ a, b })));
      setWarnings(settings.warnings ?? []);
      setAgents(list.agents ?? []);
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
  const canSave = !loading && !saving && !incomplete && !hasSelfPair;

  const updatePair = (index: number, side: 'a' | 'b', value: string) => {
    setPairs((prev) => prev.map((p, i) => (i === index ? { ...p, [side]: value } : p)));
  };

  const handleSave = async () => {
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
          {POLICIES.map((p) => (
            <label
              key={p}
              className="flex cursor-pointer items-start gap-3 px-4 py-3.5 transition-colors hover:bg-surface-hover"
            >
              <input
                type="radio"
                name="delegation-policy"
                className="mt-1 accent-primary"
                checked={policy === p}
                disabled={loading}
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
        {policy === 'open' && (
          <p className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-foreground">
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-warning" />
            {t('settings.delegation.policy.open.warning')}
          </p>
        )}
      </SettingsSection>

      {/* ② Cross-department pairs */}
      <SettingsSection
        title={t('settings.delegation.pairs')}
        description={t('settings.delegation.pairs.desc')}
      >
        <SettingsCard>
          {pairs.length === 0 && (
            <p className="px-4 py-4 text-center text-xs text-muted-foreground">
              {t('settings.delegation.pairs.empty')}
            </p>
          )}
          {pairs.map((pair, index) => (
            <div
              key={index}
              className="flex flex-wrap items-center gap-2 px-4 py-3 sm:flex-nowrap"
            >
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <OptionSelect
                  value={pair.a}
                  showRaw={false}
                  options={agentOptions}
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
                  onChange={(v) => updatePair(index, 'b', v)}
                />
                {agentDepartment(pair.b) && (
                  <Badge variant="secondary" className="shrink-0">{agentDepartment(pair.b)}</Badge>
                )}
              </div>
              <Button
                variant="ghost"
                size="sm"
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
              disabled={loading || agents.length < 2}
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
