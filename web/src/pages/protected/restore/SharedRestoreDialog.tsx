import { useEffect, useId, useState } from 'react';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Skeleton,
} from '@/components/mds';
import {
  ACCESS_LEVELS,
  adminRestoreApi,
  isValidEmployeeLabel,
  normalizeRootState,
  type AccessLevel,
  type QuarantineRow,
  type SharedRestoreOptions,
} from '@/lib/isolation-restore-api';
import { RestoreErrorNotice, useRestoreT } from './restore-ui';
import { isStepUpCancelled, type WithToken } from './with-token';

/** A member can be restored at their snapshot level or lower, never higher. */
export function allowedLevels(snapshot: AccessLevel): AccessLevel[] {
  const index = ACCESS_LEVELS.indexOf(snapshot);
  return index < 0 ? ['viewer'] : ACCESS_LEVELS.slice(index);
}

type MemberPick = { include: boolean; level: AccessLevel };

/**
 * Shared employee restore (or, when its runtime is gone, a take-over by a
 * new Shared employee). The member list comes from the tombstone snapshot:
 * levels are capped at what each member had, inactive accounts are dropped,
 * and each tenant account route is ticked on purpose. Another Admin approves.
 */
export function SharedRestoreDialog({
  row,
  open,
  onOpenChange,
  withToken,
  onFiled,
}: {
  row: QuarantineRow | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  withToken: WithToken;
  onFiled: () => void;
}) {
  const t = useRestoreT();
  const labelId = useId();
  const [options, setOptions] = useState<SharedRestoreOptions | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [members, setMembers] = useState<Record<string, MemberPick>>({});
  const [routes, setRoutes] = useState<Record<string, boolean>>({});
  const [label, setLabel] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const adopt = row ? normalizeRootState(row.root_state) === 'root_gone' : false;

  useEffect(() => {
    if (!open || !row) return;
    let active = true;
    setOptions(null);
    setLoadError(null);
    setError(null);
    setLabel(row.label);
    adminRestoreApi
      .sharedOptions(row.employee_id, row.incarnation)
      .then((res) => {
        if (!active) return;
        setOptions(res);
        setMembers(Object.fromEntries(res.members.map((m) => [m.user_id, { include: m.active, level: m.access_level }])));
        setRoutes(Object.fromEntries(res.routes.map((r) => [`${r.account}#${r.generation}`, true])));
      })
      .catch((e) => { if (active) setLoadError(e); });
    return () => { active = false; };
  }, [open, row]);

  if (!row) return null;
  const name = row.display_name || row.label;
  const chosenMembers = options?.members
    .filter((m) => m.active && members[m.user_id]?.include)
    .map((m) => ({ user_id: m.user_id, access_level: members[m.user_id].level })) ?? [];
  const chosenRoutes = options?.routes
    .filter((r) => routes[`${r.account}#${r.generation}`])
    .map((r) => ({ account: r.account, generation: r.generation })) ?? [];
  const labelOk = !adopt || isValidEmployeeLabel(label);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await withToken((token) =>
        adopt
          ? adminRestoreApi.sharedAdoptionRequest(row.employee_id, row.incarnation, chosenMembers, chosenRoutes, token,
            label !== row.label ? label : undefined)
          : adminRestoreApi.sharedRestoreRequest(row.employee_id, row.incarnation, chosenMembers, chosenRoutes, token),
      );
      onFiled();
      onOpenChange(false);
    } catch (e) {
      if (!isStepUpCancelled(e)) setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t(adopt ? 'restoreFlow.shared.adoptTitle' : 'restoreFlow.shared.title', { name })}</DialogTitle>
          <DialogDescription>{t(adopt ? 'restoreFlow.shared.adoptDesc' : 'restoreFlow.shared.desc')}</DialogDescription>
        </DialogHeader>

        {loadError != null && <RestoreErrorNotice error={loadError} />}
        {!options && loadError == null && <Skeleton className="h-32 w-full" />}

        {options && (
          <div className="space-y-4">
            {adopt && (
              <div className="grid gap-1.5">
                <label htmlFor={labelId} className="text-sm font-medium">{t('restoreFlow.adopt.newLabel')}</label>
                <Input id={labelId} value={label} maxLength={64} spellCheck={false} className="font-mono sm:w-72"
                  aria-invalid={!labelOk} onChange={(e) => setLabel(e.target.value.trim().toLowerCase())} />
                <p className={labelOk ? 'text-xs text-muted-foreground' : 'text-xs text-destructive'}>{t('myPersonal.new.labelRule')}</p>
              </div>
            )}

            <fieldset className="space-y-2">
              <legend className="text-sm font-medium">{t('restoreFlow.shared.members')}</legend>
              <p className="text-xs text-muted-foreground">{t('restoreFlow.shared.levelCap')}</p>
              {options.members.length === 0 && <p className="text-sm text-muted-foreground">{t('restoreFlow.shared.noMembers')}</p>}
              <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
                {options.members.map((m) => {
                  const pick = members[m.user_id] ?? { include: false, level: m.access_level };
                  const who = m.name || m.user_id;
                  const boxId = `${labelId}-m-${m.user_id}`;
                  return (
                    <li key={m.user_id} className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-2 text-sm">
                      <input
                        id={boxId}
                        type="checkbox"
                        className="size-4 accent-primary"
                        checked={m.active && pick.include}
                        disabled={!m.active}
                        onChange={(e) => setMembers((prev) => ({ ...prev, [m.user_id]: { ...pick, include: e.target.checked } }))}
                      />
                      <label htmlFor={boxId} className="min-w-0 flex-1 truncate" title={who}>{who}</label>
                      {m.is_primary && <Badge variant="secondary">{t('restoreFlow.shared.primary')}</Badge>}
                      {m.active ? (
                        <select
                          aria-label={t('restoreFlow.shared.levelFor', { name: who })}
                          className="h-8 rounded-lg border border-input bg-background px-2 text-sm focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
                          value={pick.level}
                          disabled={!pick.include}
                          onChange={(e) => setMembers((prev) => ({ ...prev, [m.user_id]: { ...pick, level: e.target.value as AccessLevel } }))}
                        >
                          {allowedLevels(m.access_level).map((level) => (
                            <option key={level} value={level}>{t(`restoreFlow.level.${level}`)}</option>
                          ))}
                        </select>
                      ) : (
                        <span className="w-full text-xs text-muted-foreground sm:w-auto">
                          {t('restoreFlow.shared.dropped')}
                          {m.dropped_reason ? ` · ${m.dropped_reason}` : ''}
                        </span>
                      )}
                    </li>
                  );
                })}
              </ul>
            </fieldset>

            <fieldset className="space-y-2">
              <legend className="text-sm font-medium">{t('restoreFlow.shared.routes')}</legend>
              <p className="text-xs text-muted-foreground">{t('restoreFlow.shared.routesHint')}</p>
              {options.routes.length === 0 && <p className="text-sm text-muted-foreground">{t('restoreFlow.shared.noRoutes')}</p>}
              <ul className="space-y-1.5">
                {options.routes.map((r) => {
                  const key = `${r.account}#${r.generation}`;
                  const boxId = `${labelId}-r-${key}`;
                  return (
                    <li key={key} className="flex items-center gap-2 text-sm">
                      <input id={boxId} type="checkbox" className="size-4 accent-primary" checked={routes[key] ?? false}
                        onChange={(e) => setRoutes((prev) => ({ ...prev, [key]: e.target.checked }))} />
                      <label htmlFor={boxId} className="min-w-0 break-words">
                        {r.label || t('restoreFlow.shared.routeLabel', { provider: r.provider, kind: r.kind })}
                      </label>
                    </li>
                  );
                })}
              </ul>
            </fieldset>

            <p className="text-sm">{t('restoreFlow.shared.secondAdmin')}</p>
          </div>
        )}

        <RestoreErrorNotice error={error} />

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>{t('restoreFlow.common.cancel')}</Button>
          <Button variant="brand" disabled={busy || !options || !labelOk} onClick={() => void submit()}>
            {busy ? t('restoreFlow.common.sending') : t(adopt ? 'restoreFlow.shared.submitAdopt' : 'restoreFlow.shared.submit')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
