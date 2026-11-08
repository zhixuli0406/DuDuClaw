import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertCircle, Clock } from 'lucide-react';
import { api, type OrgChangeFields, type OrgPendingChange, type OrgPlacement } from '@/lib/api';
import {
  Badge,
  Button,
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';
import { toast, formatError } from '@/lib/toast';
import { isNewFeature } from '@/apps/registry';
import { useSystemStore } from '@/stores/system-store';
import {
  normPlacement,
  orgErrorCode,
  orgErrorField,
  orgErrorMessage,
  wouldCycle,
} from '@/lib/org-change';

/** The release this editing surface shipped in (drives the 新功能 badge). */
export const ORG_EDIT_NEW_IN = '1.81.0';

export interface PlacementAgent {
  name: string;
  display_name?: string;
  role?: string;
  department?: string;
  reports_to?: string;
}

/** Drag-and-drop proposes a manager; `nonce` makes a repeat drop re-apply. */
export interface PlacementDraft {
  agent: string;
  manager: string;
  nonce: number;
}

interface Props {
  agent: PlacementAgent;
  agents: ReadonlyArray<PlacementAgent>;
  isAdmin: boolean;
  /** A waiting (not yet decided) request for this employee, if any. */
  pending?: OrgPendingChange;
  draft?: PlacementDraft | null;
  /** Reload the roster + waiting requests after something changed. */
  onChanged: () => void;
  /** Re-fetch the roster (the 重新整理 button of a missing manager). */
  onRefresh: () => void;
}

const MANAGER_FIELD_CODES = new Set(['cycle', 'self_report', 'target_not_found']);
const DEPARTMENT_FIELD_CODES = new Set(['invalid_department', 'unknown_department']);

interface FieldError {
  code: string;
  message: string;
  /** Selection the error was reported for; it clears once the selection moves on. */
  mgr: string;
  dept: string;
  /** Stays (apply stays disabled) until 重新整理, regardless of the selection. */
  sticky: boolean;
}

type Confirm =
  | { state: 'closed' }
  | { state: 'loading' }
  | { state: 'failed'; code: string | null }
  | { state: 'ready'; placement: OrgPlacement; base: string };

export function OrgPlacementPanel({ agent, agents, isAdmin, pending, draft, onChanged, onRefresh }: Props) {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );
  const version = useSystemStore((s) => s.status?.version ?? null);

  const [loaded, setLoaded] = useState<{ placement: OrgPlacement; base: string } | null>(null);
  const [departments, setDepartments] = useState<string[]>([]);
  // `null` = "leave as is" — the select follows the original until the user picks.
  const [mgrPick, setMgrPick] = useState<string | null>(null);
  const [deptPick, setDeptPick] = useState<string | null>(null);
  const [error, setError] = useState<FieldError | null>(null);
  const [notice, setNotice] = useState<'stale' | null>(null);
  const [mirrorRetry, setMirrorRetry] = useState<OrgChangeFields | null>(null);
  const [confirm, setConfirm] = useState<Confirm>({ state: 'closed' });
  const [busy, setBusy] = useState(false);

  const original = useMemo(
    () => loaded?.placement ?? normPlacement({ reports_to: agent.reports_to, department: agent.department }),
    [loaded, agent.reports_to, agent.department],
  );
  const mgr = mgrPick ?? original.reports_to;
  const dept = deptPick ?? original.department;
  const changed = mgr !== original.reports_to || dept !== original.department;

  const labelOf = useCallback(
    (id: string) => (id ? (agents.find((a) => a.name === id)?.display_name ?? id) : t('orgEdit.topLevel')),
    [agents, t],
  );
  const deptLabel = (d: string) => d || t('orgEdit.noDepartment');

  // ── loading ────────────────────────────────────────────────
  const loadPlacement = useCallback(async () => {
    try {
      const r = await api.org.placement(agent.name);
      setLoaded({ placement: normPlacement(r.placement), base: r.base });
      return r;
    } catch (e) {
      setError({ code: orgErrorCode(e) ?? 'load_failed', message: orgErrorMessage(e), mgr: '', dept: '', sticky: true });
      return null;
    }
  }, [agent.name]);

  useEffect(() => {
    void loadPlacement();
  }, [loadPlacement]);

  useEffect(() => {
    let alive = true;
    api.departments
      .list()
      .then((r) => alive && setDepartments(Array.isArray(r?.departments) ? r.departments.map((d) => d.name) : []))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, []);

  // A drag-and-drop proposes a new manager for this employee.
  useEffect(() => {
    if (draft && draft.agent === agent.name) {
      setMgrPick(draft.manager);
      setError(null);
      setNotice(null);
    }
  }, [draft, agent.name]);

  // ── derived errors ─────────────────────────────────────────
  const cycleError =
    mgr !== original.reports_to && wouldCycle(agents, agent.name, mgr)
      ? { code: mgr === agent.name ? 'self_report' : 'cycle' }
      : null;
  const shown: { code: string; message: string } | null = cycleError
    ? { ...cycleError, message: '' }
    : error && (error.sticky || (error.mgr === mgr && error.dept === dept))
      ? error
      : null;
  const hasPendingForAgent = !!pending;

  const errorText = (e: { code: string; message: string }) => {
    const id = `orgEdit.err.${e.code}`;
    if (Object.hasOwn(intl.messages, id)) {
      return t(id, { name: labelOf(agent.name), manager: labelOf(mgr), department: deptLabel(dept) });
    }
    return e.message || t('orgEdit.err.generic');
  };

  const fields = (): OrgChangeFields => ({
    ...(mgr !== original.reports_to ? { reports_to: mgr } : {}),
    ...(dept !== original.department ? { department: dept } : {}),
  });

  const resetSelection = () => {
    setMgrPick(null);
    setDeptPick(null);
  };

  const selectMgr = (v: string) => {
    setMgrPick(v);
    setNotice(null);
  };
  const selectDept = (v: string) => {
    setDeptPick(v);
    setNotice(null);
  };

  // ── actions ────────────────────────────────────────────────
  const fail = (e: unknown, snapshot: { mgr: string; dept: string }) => {
    const code = orgErrorCode(e);
    if (code === 'stale_base') {
      const current = orgErrorField<OrgPlacement>(e, 'current');
      const currentBase = orgErrorField<string>(e, 'current_base');
      if (current && currentBase) setLoaded({ placement: normPlacement(current), base: currentBase });
      else void loadPlacement();
      setNotice('stale');
      setError(null);
      onChanged();
      return;
    }
    if (code === 'mirror_sync_failed') {
      // The change itself is saved; only the employee-settings copy lags.
      setMirrorRetry({
        reports_to: snapshot.mgr,
        department: snapshot.dept,
      });
      resetSelection();
      void loadPlacement();
      onChanged();
      return;
    }
    if (code) {
      setError({
        code,
        message: orgErrorMessage(e),
        ...snapshot,
        sticky: code === 'org_authority_unavailable' || code === 'target_not_found' || code === 'agent_not_found',
      });
      return;
    }
    toast.error(t('orgEdit.saveFailed', { message: formatError(e) }));
  };

  const successToast = (before: OrgPlacement, after: OrgPlacement) => {
    const name = labelOf(agent.name);
    if (after.reports_to !== before.reports_to) {
      toast.success(
        after.reports_to
          ? t('orgEdit.toast.appliedManager', { name, manager: labelOf(after.reports_to) })
          : t('orgEdit.toast.appliedTop', { name }),
      );
    } else {
      toast.success(t('orgEdit.toast.appliedDepartment', { name, department: deptLabel(after.department) }));
    }
  };

  const openConfirm = async () => {
    setConfirm({ state: 'loading' });
    const r = await api.org
      .placement(agent.name)
      .then((v) => ({ placement: normPlacement(v.placement), base: v.base }))
      .catch((e: unknown) => ({ failed: orgErrorCode(e) }));
    if ('failed' in r) {
      setConfirm({ state: 'failed', code: r.failed });
      return;
    }
    setLoaded(r);
    setConfirm({ state: 'ready', placement: r.placement, base: r.base });
  };

  const doApply = async () => {
    if (confirm.state !== 'ready' || busy) return;
    const snapshot = { mgr, dept };
    // The fields are relative to the placement the admin just confirmed.
    const base = confirm.placement;
    const body: OrgChangeFields = {
      ...(mgr !== base.reports_to ? { reports_to: mgr } : {}),
      ...(dept !== base.department ? { department: dept } : {}),
    };
    setBusy(true);
    try {
      const res = await api.org.applyChange(agent.name, body, confirm.base);
      setConfirm({ state: 'closed' });
      const after = normPlacement(res.after ?? res.placement ?? { reports_to: mgr, department: dept });
      setLoaded({ placement: after, base: res.base });
      successToast(normPlacement(res.before ?? base), after);
      resetSelection();
      setError(null);
      setNotice(null);
      setMirrorRetry(null);
      onChanged();
    } catch (e) {
      setConfirm({ state: 'closed' });
      fail(e, snapshot);
    } finally {
      setBusy(false);
    }
  };

  const retryMirror = async () => {
    if (!mirrorRetry || busy) return;
    setBusy(true);
    try {
      // Re-applying the placement the authority already has re-syncs the copy.
      const p = await api.org.placement(agent.name);
      await api.org.applyChange(agent.name, mirrorRetry, p.base);
      toast.success(t('orgEdit.toast.mirrorSynced'));
      setMirrorRetry(null);
      void loadPlacement();
      onChanged();
    } catch (e) {
      if (orgErrorCode(e) === 'mirror_sync_failed') {
        toast.error(t('orgEdit.err.mirror_sync_failed'));
      } else {
        fail(e, { mgr, dept });
      }
    } finally {
      setBusy(false);
    }
  };

  const propose = async () => {
    if (busy) return;
    const snapshot = { mgr, dept };
    setBusy(true);
    try {
      await api.org.proposeChange(agent.name, fields());
      toast.success(t('orgEdit.toast.proposed'));
      resetSelection();
      setError(null);
      setNotice(null);
      onChanged();
    } catch (e) {
      fail(e, snapshot);
    } finally {
      setBusy(false);
    }
  };

  const refresh = () => {
    setError(null);
    setNotice(null);
    onRefresh();
    void loadPlacement();
  };

  // ── render ─────────────────────────────────────────────────
  const managerOptions = agents.filter((a) => a.name !== agent.name);
  const deptOptions = Array.from(new Set(['', ...departments, original.department, dept]));
  const mgrError = shown && MANAGER_FIELD_CODES.has(shown.code) ? shown : null;
  const deptError = shown && DEPARTMENT_FIELD_CODES.has(shown.code) ? shown : null;
  const formError = shown && !mgrError && !deptError ? shown : null;
  const blocked = !!shown;
  const applyDisabled = !changed || blocked || busy || (!isAdmin && hasPendingForAgent);
  const showNew = isNewFeature(ORG_EDIT_NEW_IN, version);
  const selectClass =
    'h-9 w-full rounded-lg border border-input bg-background px-2.5 text-sm text-foreground ' +
    'focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50 aria-[invalid=true]:border-destructive';
  const ids = { mgr: `org-mgr-${agent.name}`, dept: `org-dept-${agent.name}` };

  const errorBox = (id: string, e: { code: string; message: string }) => (
    <p
      id={id}
      role="alert"
      className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive"
    >
      <AlertCircle className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
      <span>{errorText(e)}</span>
    </p>
  );

  return (
    <section
      aria-label={t('orgEdit.title')}
      className="space-y-3 rounded-xl border border-surface-border p-4"
      data-testid="org-placement-panel"
    >
      <h4 className="flex items-center gap-2 text-sm font-medium text-foreground">
        {t('orgEdit.title')}
        {showNew && <Badge variant="outline">{t('nav.badge.new')}</Badge>}
      </h4>

      <div className="space-y-1.5">
        <label htmlFor={ids.mgr} className="text-xs font-medium text-muted-foreground">
          {t('orgEdit.manager')}
        </label>
        <select
          id={ids.mgr}
          className={selectClass}
          value={mgr}
          onChange={(e) => selectMgr(e.target.value)}
          aria-invalid={mgrError ? true : undefined}
          aria-describedby={mgrError ? `${ids.mgr}-err` : undefined}
        >
          <option value="">{t('orgEdit.topLevel')}</option>
          {managerOptions.map((a) => (
            <option key={a.name} value={a.name}>
              {a.display_name ?? a.name}
              {a.department ? `（${a.department}）` : ''}
            </option>
          ))}
          {mgr && !managerOptions.some((a) => a.name === mgr) && <option value={mgr}>{mgr}</option>}
        </select>
        <p className="text-xs text-muted-foreground">
          {mgr !== original.reports_to
            ? t('orgEdit.original', { value: labelOf(original.reports_to) })
            : t('orgEdit.unchanged')}
        </p>
        {mgrError && errorBox(`${ids.mgr}-err`, mgrError)}
      </div>

      <div className="space-y-1.5">
        <label htmlFor={ids.dept} className="text-xs font-medium text-muted-foreground">
          {t('orgEdit.department')}
        </label>
        <select
          id={ids.dept}
          className={selectClass}
          value={dept}
          onChange={(e) => selectDept(e.target.value)}
          aria-invalid={deptError ? true : undefined}
          aria-describedby={deptError ? `${ids.dept}-err` : undefined}
        >
          {deptOptions.map((d) => (
            <option key={d || '__none__'} value={d}>
              {deptLabel(d)}
            </option>
          ))}
        </select>
        <p className="text-xs text-muted-foreground">
          {dept !== original.department
            ? t('orgEdit.original', { value: deptLabel(original.department) })
            : t('orgEdit.unchanged')}
        </p>
        {deptError && errorBox(`${ids.dept}-err`, deptError)}
      </div>

      {!isAdmin && (
        <p role="note" className="rounded-lg bg-muted px-3 py-2 text-xs text-foreground">
          {hasPendingForAgent ? t('orgEdit.err.pending_exists') : t('orgEdit.managerNote')}
        </p>
      )}
      {formError && errorBox('org-form-error', formError)}
      {notice === 'stale' && (
        <p role="alert" className="flex items-start gap-2 rounded-md border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-foreground">
          <Clock className="mt-0.5 size-3.5 shrink-0 text-warning" aria-hidden="true" />
          {t('orgEdit.err.stale_base')}
        </p>
      )}
      {mirrorRetry && (
        <div role="alert" className="space-y-2 rounded-md border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-foreground">
          <p>{t('orgEdit.err.mirror_sync_failed')}</p>
          <Button size="sm" variant="outline" onClick={() => void retryMirror()} disabled={busy}>
            {t('orgEdit.retry')}
          </Button>
        </div>
      )}

      <div className="flex justify-end gap-2">
        {shown && (shown.code === 'target_not_found' || shown.code === 'org_authority_unavailable' || shown.code === 'agent_not_found') ? (
          <Button variant="outline" size="sm" onClick={refresh}>
            {t('common.refresh')}
          </Button>
        ) : (
          <Button
            variant="outline"
            size="sm"
            disabled={!changed || busy}
            onClick={() => {
              resetSelection();
              setError(null);
              setNotice(null);
            }}
          >
            {t('common.cancel')}
          </Button>
        )}
        {isAdmin ? (
          <Button variant="brand" size="sm" disabled={applyDisabled} onClick={() => void openConfirm()}>
            {t('orgEdit.apply')}
          </Button>
        ) : (
          <Button variant="brand" size="sm" disabled={applyDisabled} onClick={() => void propose()}>
            {t('orgEdit.submit')}
          </Button>
        )}
      </div>

      {/* B2 — Admin confirmation: 調整前 / 調整後. */}
      <Dialog open={confirm.state !== 'closed'} onOpenChange={(open) => !open && !busy && setConfirm({ state: 'closed' })}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{t('orgEdit.confirm.title', { name: labelOf(agent.name) })}</DialogTitle>
            <DialogDescription>{t('orgEdit.confirm.desc')}</DialogDescription>
          </DialogHeader>
          {confirm.state === 'loading' && (
            <p role="status" className="text-sm text-muted-foreground">{t('orgEdit.confirm.loading')}</p>
          )}
          {confirm.state === 'failed' && (
            <p role="alert" className="text-sm text-destructive">
              {confirm.code && Object.hasOwn(intl.messages, `orgEdit.err.${confirm.code}`)
                ? t(`orgEdit.err.${confirm.code}`)
                : t('orgEdit.confirm.loadFailed')}
            </p>
          )}
          {confirm.state === 'ready' && (
            <div className="grid grid-cols-2 gap-3 text-sm">
              <div className="space-y-2 rounded-lg border border-surface-border p-3" data-testid="org-confirm-before">
                <p className="text-xs font-medium text-muted-foreground">{t('orgEdit.confirm.before')}</p>
                <PlacementFacts t={t} labelOf={labelOf} deptLabel={deptLabel} value={confirm.placement} />
              </div>
              <div className="space-y-2 rounded-lg border border-brand bg-surface-hover p-3" data-testid="org-confirm-after">
                <p className="text-xs font-medium text-brand">{t('orgEdit.confirm.after')}</p>
                <PlacementFacts
                  t={t}
                  labelOf={labelOf}
                  deptLabel={deptLabel}
                  value={{ reports_to: mgr, department: dept }}
                  against={confirm.placement}
                />
              </div>
            </div>
          )}
          <DialogFooter>
            <DialogClose render={<Button variant="outline" disabled={busy}>{t('common.cancel')}</Button>} />
            <Button variant="brand" disabled={confirm.state !== 'ready' || busy} onClick={() => void doApply()}>
              {t('orgEdit.confirm.submit')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}

function PlacementFacts({
  t,
  labelOf,
  deptLabel,
  value,
  against,
}: {
  t: (id: string, values?: Record<string, string | number>) => string;
  labelOf: (id: string) => string;
  deptLabel: (d: string) => string;
  value: OrgPlacement;
  /** When set, a field equal to this one is marked （不變）. */
  against?: OrgPlacement;
}) {
  return (
    <dl className="space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <dt className="text-xs text-muted-foreground">{t('orgEdit.manager')}</dt>
        <dd className="min-w-0 truncate font-medium">
          {labelOf(value.reports_to)}
          {against && against.reports_to === value.reports_to && (
            <span className="font-normal text-muted-foreground">{t('orgEdit.confirm.unchangedSuffix')}</span>
          )}
        </dd>
      </div>
      <div className="flex items-center justify-between gap-2">
        <dt className="text-xs text-muted-foreground">{t('orgEdit.department')}</dt>
        <dd className="min-w-0 truncate font-medium">
          {deptLabel(value.department)}
          {against && against.department === value.department && (
            <span className="font-normal text-muted-foreground">{t('orgEdit.confirm.unchangedSuffix')}</span>
          )}
        </dd>
      </div>
    </dl>
  );
}
