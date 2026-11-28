import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, ArrowLeftRight, Plus, Trash2 } from 'lucide-react';
import { api, type AgentDetail, type DelegationPolicy, type DelegationRole } from '@/lib/api';
import { toast } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import {
  Badge,
  Button,
  ErrorState,
  SettingsCard,
  SettingsSection,
  SettingsSaveState,
  useErrorMessage,
} from '@/components/mds';
import { AdminAiCard } from './AdminAiCard';
import { isSettingsConflict, settingsRefusal, settingsWarningMessage } from '@/lib/settings-refusal';
import { RiskConfirmDialog, OptionSelect, type SelectOption } from '@/components/settings/controls';

export const DELEGATION_POLICIES: DelegationPolicy[] = ['department', 'hierarchy', 'open', 'none'];

/** A whitelist row while it is being edited — either side may still be blank. */
type PairDraft = { a: string; b: string; direction?: 'two_way' | 'one_way' };
const ROLES: DelegationRole[] = ['admin', 'manager'];

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
  const errorText = useErrorMessage();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');

  // Placeholders only: nothing below is shown as a setting or submitted
  // until `loaded` is true.
  const [policy, setPolicy] = useState<DelegationPolicy>('department');
  const [roles, setRoles] = useState<DelegationRole[]>([]);
  const [savedRoles, setSavedRoles] = useState<DelegationRole[]>([]);
  const [pendingRole, setPendingRole] = useState<DelegationRole | null>(null);
  const [pairs, setPairs] = useState<PairDraft[]>([]);
  const [warnings, setWarnings] = useState<unknown[]>([]);
  const [agents, setAgents] = useState<AgentDetail[]>([]);
  const [loading, setLoading] = useState(true);
  // True only after the latest load succeeded.
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const hashRef = useRef<string | null>(null);
  const [conflict, setConflict] = useState(false);
  // P05: a failed read left the radio group on its `department` default and an
  // empty pair list — visually identical to a real, saved configuration.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  useEffect(() => { setSaveError(null); }, [policy, roles, pairs]);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    setLoadError(null);
    setSaveError(null);
    setLoaded(false);
    setLoading(true);
    hashRef.current = null;
    try {
      const [settings, list] = await Promise.all([
        api.delegation.get(),
        api.agents.list(),
      ]);
      hashRef.current = typeof settings.hash === 'string' && settings.hash ? settings.hash : null;
      setConflict(false);
      setPolicy(settings.policy);
      setPairs([...settings.allow.map(([a, b]) => ({ a, b })), ...(settings.allow_one_way ?? []).map(([a, b]) => ({ a, b, direction: 'one_way' as const }))]);
      const loadedRoles = (settings.unrestricted_roles ?? []).filter((role) => ROLES.includes(role));
      setRoles(loadedRoles);
      setSavedRoles(loadedRoles);
      setPendingRole(null);
      setWarnings(Array.isArray(settings.warnings) ? settings.warnings : [null]);
      setAgents(list.agents ?? []);
      setLoaded(true);
    } catch (e) {
      console.warn('[api]', e);
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl, errorText]);

  useEffect(() => {
    if (!isAdmin) {
      setLoading(false);
      return;
    }
    void load();
  }, [isAdmin, load]);

  if (!isAdmin) return null;

  const agentLabel = (name: string) =>
    agents.find((a) => a.name === name)?.display_name || t('agentDetail.delegation.unknownPeer');
  const agentDepartment = (name: string) =>
    agents.find((a) => a.name === name)?.department || '';

  const agentOptions: SelectOption[] = [
    { value: '', label: t('settings.delegation.pair.pick') },
    ...agents.map((a) => ({
      value: a.name,
      label: a.department
        ? `${a.display_name || t('agentDetail.delegation.unknownPeer')}（${a.department}）`
        : (a.display_name || t('agentDetail.delegation.unknownPeer')),
    })),
  ];

  const incomplete = pairs.some((p) => !p.a || !p.b);
  const invalidRows = new Set<number>();
  let hasDuplicate = false;
  pairs.forEach((pair, index) => {
    if (!pair.a || !pair.b) return;
    if (pair.a === pair.b) invalidRows.add(index);
    pairs.slice(0, index).forEach((other, otherIndex) => {
      const same = pair.a === other.a && pair.b === other.b;
      const reversed = pair.a === other.b && pair.b === other.a;
      if (same || (reversed && (pair.direction !== 'one_way' || other.direction !== 'one_way'))) {
        hasDuplicate = true;
        invalidRows.add(index);
        invalidRows.add(otherIndex);
      }
    });
  });
  // Only a refusal naming both visible employees identifies a server-side row.
  const refusal = settingsRefusal(saveError, errorText(saveError));
  const namedRows = pairs.flatMap((pair, index) => {
    const a = agents.find((agent) => agent.name === pair.a)?.display_name;
    const b = agents.find((agent) => agent.name === pair.b)?.display_name;
    return a && b && refusal.includes(a) && refusal.includes(b) ? [index] : [];
  });
  if (saveError != null && namedRows.length === 1) invalidRows.add(namedRows[0]);
  const hasSelfPair = pairs.some((p) => p.a && p.a === p.b);
  const tooManyPairs = pairs.length > 200;
  // Editing is possible only on a successfully loaded setting.
  const editable = hashRef.current != null && loaded && !loading && loadError == null && !conflict;
  const addsRole = roles.some((role) => !savedRoles.includes(role));
  const canSave = editable && !saving && !incomplete && !hasSelfPair && !hasDuplicate && !tooManyPairs;

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
        expected_hash: hashRef.current!,
        policy,
        allow: pairs.filter((p) => p.direction !== 'one_way').map((p) => [p.a, p.b] as [string, string]),
        allow_one_way: pairs.filter((p) => p.direction === 'one_way').map((p) => [p.a, p.b] as [string, string]),
        unrestricted_roles: roles,
        ...(addsRole ? { acknowledge_risk: true } : {}),
      });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
      // Re-read so any server-side cleaning (dedup, dropped self-pairs) is
      // what the operator ends up looking at.
      await load();
    } catch (e) {
      console.warn('[api]', e);
      setSaveError(e);
      if (isSettingsConflict(settingsRefusal(e, errorText(e)))) setConflict(true);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: errorText(e) }));
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
          <ul className="list-disc space-y-1 pl-5 text-muted-foreground">
            {warnings.map((warning, index) => <li key={index}>{settingsWarningMessage(warning, t('settings.delegation.safeWarning'))}</li>)}
          </ul>
        </div>
      )}

      {conflict && <div className="space-y-2">
        <p role="alert" className="text-sm text-destructive">{refusal}</p>
        <Button disabled={loading || saving} onClick={() => void load()}>{t('settings.delegation.reload')}</Button>
      </div>}

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
                disabled={!editable || saving}
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

      <SettingsSection title={t('settings.delegation.roles.title')}>
        <p className="text-xs text-muted-foreground">{t('settings.delegation.roles.help')}</p>
        <p className="text-xs text-muted-foreground">{t('settings.delegation.saveImpact')}</p>
        <SettingsCard>
          {ROLES.map((role) => (
            <label key={role} className="flex items-center gap-3 px-4 py-3">
              <input type="checkbox" aria-label={t(`settings.delegation.roles.${role}`)} className="accent-primary" checked={loaded && roles.includes(role)} disabled={!editable || saving}
                onChange={(e) => {
                  if (e.target.checked) setPendingRole(role);
                  else setRoles((prev) => prev.filter((r) => r !== role));
                }} />
              <span><span className="block text-sm">{t(`settings.delegation.roles.${role}`)}</span><span className="block text-xs text-muted-foreground">{t(`settings.delegation.roles.${role}.help`)}</span></span>
            </label>
          ))}
        </SettingsCard>
        {loaded && policy === 'none' && roles.length > 0 && (
          <p role="alert" className="text-xs text-warning">{t('settings.delegation.roles.noneWarning')}</p>
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
              className={`px-4 py-3 ${invalidRows.has(index) ? 'rounded-md ring-1 ring-inset ring-destructive' : ''}`}
              data-pair-row={index}
            >
              <div className="flex flex-wrap items-center gap-2 sm:flex-nowrap">
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <OptionSelect
                  value={pair.a}
                  showRaw={false}
                  options={agentOptions}
                  disabled={!editable || saving}
                  onChange={(v) => updatePair(index, 'a', v)}
                />
                {agentDepartment(pair.a) && (
                  <Badge variant="secondary" className="shrink-0">{agentDepartment(pair.a)}</Badge>
                )}
              </div>
              <label htmlFor={`delegation-direction-${index}`} className="sr-only">{t('settings.delegation.pair.direction')}</label>
              <OptionSelect id={`delegation-direction-${index}`} className="w-auto max-w-40 shrink-0" value={pair.direction ?? 'two_way'} showRaw={false} disabled={!editable || saving}
                options={[{ value: 'two_way', label: t('settings.delegation.pair.twoWay') }, { value: 'one_way', label: t('settings.delegation.pair.oneWay') }]}
                onChange={(value) => setPairs((prev) => prev.map((p, i) => i === index ? { ...p, direction: value as 'two_way' | 'one_way' } : p))} />
              {pair.direction === 'one_way' && (
                <Button variant="ghost" size="sm" disabled={!editable || saving} aria-label={t('settings.delegation.pair.swap')}
                  onClick={() => setPairs((prev) => prev.map((p, i) => i === index ? { ...p, a: p.b, b: p.a } : p))}>
                  <ArrowLeftRight className="size-4" />
                </Button>
              )}
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <OptionSelect
                  value={pair.b}
                  showRaw={false}
                  options={agentOptions}
                  disabled={!editable || saving}
                  onChange={(v) => updatePair(index, 'b', v)}
                />
                {agentDepartment(pair.b) && (
                  <Badge variant="secondary" className="shrink-0">{agentDepartment(pair.b)}</Badge>
                )}
              </div>
              <Button
                variant="ghost"
                size="sm"
                disabled={!editable || saving}
                aria-label={intl.formatMessage(
                  { id: 'settings.delegation.pair.remove' },
                  { a: agentLabel(pair.a), b: agentLabel(pair.b) },
                )}
                onClick={() => setPairs((prev) => prev.filter((_, i) => i !== index))}
              >
                <Trash2 className="size-4" />
              </Button>
              </div>
              {pair.direction === 'one_way' && (
                <p className="mt-2 text-xs text-muted-foreground">{intl.formatMessage({ id: 'settings.delegation.pair.oneWayHelp' }, { a: agentLabel(pair.a), b: agentLabel(pair.b) })}</p>
              )}
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

      <div className="space-y-3">
        {((saveError != null && !conflict) || tooManyPairs || hasSelfPair || hasDuplicate) && (
          <p role="alert" className="text-sm text-destructive">
            {tooManyPairs ? t('settings.delegation.pairs.limit') : hasSelfPair ? t('settings.delegation.pair.same') : hasDuplicate ? t('settings.delegation.pair.duplicate') : refusal}
          </p>
        )}
      <div className="flex flex-wrap items-center justify-end gap-3">
        {!loaded && !loading && (
          <span className="text-xs text-warning">{t('settings.delegation.saveBlocked')}</span>
        )}
        {incomplete && (
          <span className="text-xs text-warning">{t('settings.delegation.pair.incomplete')}</span>
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
      <RiskConfirmDialog
        open={pendingRole !== null}
        onClose={() => setPendingRole(null)}
        onConfirm={() => {
          if (pendingRole) setRoles((prev) => [...prev, pendingRole]);
          setPendingRole(null);
        }}
        title={intl.formatMessage({ id: 'settings.delegation.roles.confirmTitle' }, { role: pendingRole ? t(`settings.delegation.roles.${pendingRole}`) : '' })}
        body={intl.formatMessage({ id: 'settings.delegation.roles.confirmBody' }, { role: pendingRole ? t(`settings.delegation.roles.${pendingRole}`) : '' })}
        acknowledgeLabel={t('settings.delegation.roles.confirmAcknowledge')}
        auditNote={t('settings.delegation.roles.confirmAudit')}
        confirmLabel={t('settings.delegation.roles.confirm')}
      />
      <AdminAiCard />
    </div>
  );
}
