import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link } from 'react-router';
import { ArchiveRestore, ChevronLeft, ShieldCheck } from 'lucide-react';
import { Button, Skeleton } from '@/components/mds';
import { useStepUpToken } from '@/components/step-up/useStepUpToken';
import { api } from '@/lib/api';
import {
  adminRestoreApi,
  type IsolationEmployeeList,
  type QuarantineRow,
  type RestoreRequestRow,
} from '@/lib/isolation-restore-api';
import { PROTECTED_MODE_PATH, PageTrail } from '../shared';
import { FileForOwnerDialog } from './FileForOwnerDialog';
import { PurgeDialog, type PurgeMode } from './PurgeDialog';
import { QuarantineTable, type RowAction } from './QuarantineTable';
import { RegrantList } from './RegrantList';
import { RestoreApproveCard } from './RestoreApproveCard';
import { RetireDialog } from './RetireDialog';
import { SharedRestoreDialog } from './SharedRestoreDialog';
import { NeedsVerifyProvider, RestoreErrorNotice, RestorePeopleProvider, useRestoreT } from './restore-ui';
import { makeWithToken, type WithToken } from './with-token';

/** Route of the Admin restore screen; part A's AI 員工 tab links here. */
export const ADMIN_RESTORE_PATH = `${PROTECTED_MODE_PATH}/restore`;

/**
 * The row-level restore / purge / retire dialogs, opened from any list of
 * quarantine rows (this screen's table, or the AI 員工 tab's retired rows).
 */
export function QuarantineRowDialogs({
  active,
  onClose,
  withToken,
  onDone,
}: {
  active: { row: QuarantineRow; action: RowAction } | null;
  onClose: () => void;
  withToken: WithToken;
  onDone: () => void;
}) {
  const close = (open: boolean) => { if (!open) onClose(); };
  const purgeMode: PurgeMode | null =
    active?.action === 'purge' ? 'ordinary'
      : active?.action === 'early_purge' ? 'early'
        : active?.action === 'confirm_early_purge' ? 'confirm_early' : null;
  return (
    <>
      <SharedRestoreDialog
        row={active?.action === 'shared_restore' ? active.row : null}
        open={active?.action === 'shared_restore'}
        onOpenChange={close}
        withToken={withToken}
        onFiled={onDone}
      />
      <FileForOwnerDialog
        row={active?.action === 'file_for_owner' ? active.row : null}
        open={active?.action === 'file_for_owner'}
        onOpenChange={close}
        withToken={withToken}
        onFiled={onDone}
      />
      <PurgeDialog
        row={purgeMode ? active!.row : null}
        mode={purgeMode ?? 'ordinary'}
        open={purgeMode !== null}
        onOpenChange={close}
        withToken={withToken}
        onDone={onDone}
      />
      <RetireDialog
        row={active?.action === 'retire' ? active.row : null}
        open={active?.action === 'retire'}
        onOpenChange={close}
        withToken={withToken}
        onDone={onDone}
      />
    </>
  );
}

/** users.db id → display name, from the Admin member list (names are presentation only). */
export function useMemberNames(enabled = true): ReadonlyMap<string, string> {
  const [people, setPeople] = useState<ReadonlyMap<string, string>>(new Map());
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    api.users.list()
      .then((result) => {
        if (cancelled) return;
        setPeople(new Map((result?.users ?? []).map((user) => [user.id, user.display_name || user.email])));
      })
      .catch(() => {});
    return () => { cancelled = true; };
  }, [enabled]);
  return people;
}

/**
 * 管理員 · 還原、承接與註銷 (v1.81 F5/F6e-UI, board "Restore") without page
 * chrome, so the protected-mode admin page can embed it too.
 */
export function AdminRestorePanel() {
  const t = useRestoreT();
  const people = useMemberNames();
  const [data, setData] = useState<IsolationEmployeeList | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [active, setActive] = useState<{ row: QuarantineRow; action: RowAction } | null>(null);
  const [kept, setKept] = useState<RestoreRequestRow[]>([]);
  const { ensure, dialog } = useStepUpToken();
  const withToken = useMemo(() => makeWithToken(ensure), [ensure]);
  const generation = useRef(0);

  const load = useCallback(async () => {
    const current = ++generation.current;
    try {
      const res = await adminRestoreApi.list();
      if (current !== generation.current) return;
      setData(res ?? {});
      setLoadError(null);
    } catch (e) {
      if (current === generation.current) setLoadError(e);
    }
  }, []);

  useEffect(() => {
    void load();
    return () => { generation.current += 1; };
  }, [load]);

  const quarantine = data?.restores?.quarantine ?? [];
  const serverRequests = data?.restores?.requests ?? [];
  // A decided card stays on screen (outcome + re-grant list) after the reload.
  const requests = [
    ...serverRequests,
    ...kept.filter((k) => !serverRequests.some((r) => r.request_id === k.request_id)),
  ];
  const regrants = data?.restores?.regrants ?? [];

  if (loadError != null && !data) {
    return (
      <div className="space-y-3">
        <RestoreErrorNotice error={loadError} />
        <Button variant="outline" size="sm" onClick={() => void load()}>{t('restoreFlow.common.retry')}</Button>
      </div>
    );
  }
  if (!data) return <Skeleton className="h-48 w-full" />;

  return (
    <RestorePeopleProvider value={people}>
      <NeedsVerifyProvider value={data.step_up_valid !== true}>
        <div className="space-y-8">
          {data.mfa_enrolled === false ? (
            <p role="alert" className="rounded-lg border border-warning/30 bg-warning/5 px-3 py-2 text-sm">
              {t('restoreFlow.stepUp.noMfa')}{' '}
              <Link to="/me/account" className="underline">{t('restoreFlow.stepUp.goToAccount')}</Link>
            </p>
          ) : (
            <p className="flex items-start gap-2 text-sm text-muted-foreground">
              <ShieldCheck className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
              {t(data.step_up_valid ? 'restoreFlow.page.stepUpValid' : 'restoreFlow.page.stepUpNeeded')}
            </p>
          )}
          {loadError != null && <RestoreErrorNotice error={loadError} />}

          {/* Board order: the quarantine table first, then the decisions. */}
          <section aria-labelledby="restore-quarantine-h" className="space-y-3 rounded-xl border border-surface-border bg-surface p-4 sm:p-5">
            <h2 id="restore-quarantine-h" className="text-base font-medium">{t('restoreFlow.quarantine.title')}</h2>
            <QuarantineTable rows={quarantine} onAction={(row, action) => setActive({ row, action })} />
            <p className="text-xs text-muted-foreground">{t('restoreFlow.quarantine.desc')}</p>
          </section>

          <section aria-labelledby="restore-requests-h" className="space-y-3">
            <h2 id="restore-requests-h" className="text-base font-medium">{t('restoreFlow.page.requests')}</h2>
            {requests.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('restoreFlow.page.noRequests')}</p>
            ) : (
              <div className="grid gap-4 lg:grid-cols-2">
                {requests.map((request) => (
                  <RestoreApproveCard
                    key={request.request_id}
                    request={request}
                    withToken={withToken}
                    onDecided={(r) => {
                      setKept((prev) => (prev.some((k) => k.request_id === r.request_id) ? prev : [...prev, r]));
                      void load();
                    }}
                  />
                ))}
              </div>
            )}
          </section>

          {regrants.length > 0 && (
            <section aria-labelledby="restore-regrant-h" className="space-y-3">
              <h2 id="restore-regrant-h" className="text-base font-medium">{t('restoreFlow.regrant.title')}</h2>
              {regrants.map((group) => (
                <div key={`${group.employee_id}:${group.incarnation}`} className="space-y-1">
                  <h3 className="text-sm font-medium">{group.label}</h3>
                  <RegrantList employeeId={group.employee_id} incarnation={group.incarnation} items={group.items} withToken={withToken} />
                </div>
              ))}
            </section>
          )}

          <QuarantineRowDialogs active={active} onClose={() => setActive(null)} withToken={withToken} onDone={() => void load()} />
          {dialog}
        </div>
      </NeedsVerifyProvider>
    </RestorePeopleProvider>
  );
}

export function AdminRestorePage() {
  const t = useRestoreT();
  return (
    <div className="mx-auto max-w-5xl space-y-6 p-4 sm:p-6">
      <div className="space-y-2">
        <Link to={PROTECTED_MODE_PATH} className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
          <ChevronLeft className="size-4" aria-hidden="true" /> {t('restoreFlow.page.back')}
        </Link>
        <PageTrail trail={t('restoreFlow.page.trail')} newIn="1.81.0" />
        <h1 className="flex items-center gap-2 text-xl font-semibold sm:text-2xl">
          <ArchiveRestore className="size-5 text-muted-foreground" aria-hidden="true" />
          {t('restoreFlow.page.title')}
        </h1>
        <p className="text-sm text-muted-foreground">{t('restoreFlow.page.desc')}</p>
      </div>
      <AdminRestorePanel />
    </div>
  );
}
