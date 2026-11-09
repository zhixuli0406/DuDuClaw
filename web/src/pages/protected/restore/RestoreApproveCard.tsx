import { useState } from 'react';
import { useIntl } from 'react-intl';
import { CheckCircle2, Info } from 'lucide-react';
import { Badge, Button, Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/mds';
import { useAuthStore } from '@/stores/auth-store';
import {
  adminRestoreApi,
  toMillis,
  type RestoreApproveReply,
  type RestoreRequestRow,
} from '@/lib/isolation-restore-api';
import {
  ADMIN_RESETUP_ITEMS,
  ADOPTION_CARRIED,
  ADOPTION_NOT_CARRIED,
  ItemList,
  KindBadge,
  RESTORED_ITEMS,
  RestoreErrorNotice,
  TwoLists,
  useNeedsVerify,
  usePersonLabel,
  useRestoreT,
} from './restore-ui';
import { RegrantList } from './RegrantList';
import { isStepUpCancelled, type WithToken } from './with-token';

export type ApproveState = 'ready' | 'second_admin' | 'sole_admin_self' | 'cooling';

/**
 * Who may approve: never the Admin who filed it, unless they are the only
 * Admin — then the server marks the row `self_approved` and it is a recorded
 * self-approval, which waits out a 7-day cooling period. A request filed for
 * an owner who did not confirm with a second factor always needs a different
 * Admin. "Mine" is `filed_by` = the signed-in user (`filed_by_me` from an
 * older build is still honoured). The server re-checks every rule.
 */
export function approveState(req: RestoreRequestRow, now = Date.now(), me?: string | null): ApproveState {
  const mine = req.filed_by_me === true || (me != null && req.filed_by != null && req.filed_by === me);
  const self = req.self_approved === true || (mine && req.sole_admin === true);
  if (self && !req.owner_unverified) {
    const until = toMillis(req.cooling_until ?? null);
    return until != null && until > now ? 'cooling' : 'sole_admin_self';
  }
  if (mine) return 'second_admin';
  return 'ready';
}

function count(value: boolean | number | unknown[] | undefined): number {
  if (Array.isArray(value)) return value.length;
  if (typeof value === 'number') return value;
  return value ? 1 : 0;
}

/** One restore / adoption request awaiting a decision, and its outcome. */
export function RestoreApproveCard({
  request,
  withToken,
  onDecided,
}: {
  request: RestoreRequestRow;
  withToken: WithToken;
  /** Called after any decision; the screen keeps showing this card (with
   *  its outcome and re-grant list) after the list reloads. */
  onDecided: (request: RestoreRequestRow) => void;
}) {
  const t = useRestoreT();
  const intl = useIntl();
  const person = usePersonLabel();
  const needsVerify = useNeedsVerify();
  const me = useAuthStore((s) => s.user?.id ?? null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [reply, setReply] = useState<RestoreApproveReply | null>(null);
  const [denied, setDenied] = useState(false);
  const state = approveState(request, Date.now(), me);
  const mine = request.filed_by_me === true || (me != null && request.filed_by === me);
  const name = request.display_name || request.label;
  const adoption = request.kind === 'adoption';
  const shared = request.employee_kind === 'shared';
  const coolingMs = toMillis(request.cooling_until ?? null);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      onDecided(request);
    } catch (e) {
      if (!isStepUpCancelled(e)) setError(e);
    } finally {
      setBusy(false);
    }
  };

  const approve = () => run(async () => {
    setReply(await withToken((token) => adminRestoreApi.decide(request.request_id, 'approve', token)));
  });
  const deny = () => run(async () => {
    await withToken((token) => adminRestoreApi.decide(request.request_id, 'deny', token));
    setDenied(true);
  });
  const withdraw = () => run(async () => {
    await withToken((token) => adminRestoreApi.cancel(request.request_id, token));
    setDenied(true);
  });

  const headingId = `restore-req-${request.request_id}`;

  return (
    <Card role="group" aria-labelledby={headingId}>
      <CardHeader>
        <CardTitle id={headingId} className="flex flex-wrap items-center gap-2">
          <span className="min-w-0 truncate" title={name}>
            {t(adoption ? 'restoreFlow.approve.adoptTitle' : 'restoreFlow.approve.title', { name })}
          </span>
          <KindBadge kind={request.employee_kind} />
          {request.owner_unverified && <Badge variant="destructive">{t('restoreFlow.ownerUnverified')}</Badge>}
        </CardTitle>
        <CardDescription>
          {request.filed_by_admin
            ? t('restoreFlow.approve.filedByAdmin', { who: person(request.filed_by, request.filed_by_name) })
            : t('restoreFlow.approve.filedByOwner', { who: person(request.filed_by ?? request.owner_user_id, request.filed_by_name) })}
          {request.created_at != null && toMillis(request.created_at) != null && (
            <> · {intl.formatDate(toMillis(request.created_at)!, { dateStyle: 'medium', timeStyle: 'short' })}</>
          )}
        </CardDescription>
      </CardHeader>

      <CardContent className="space-y-4">
        {adoption ? (
          <section aria-label={t('restoreFlow.approve.adoptionProposal')} className="space-y-2">
            {request.target_label && (
              <p className="text-sm">
                {t('restoreFlow.approve.newLabel')} <span className="font-mono">{request.target_label}</span>
              </p>
            )}
            <TwoLists
              leftTitle={t('restoreFlow.adopt.carried')}
              left={<ItemList keys={request.carried?.length ? request.carried : ADOPTION_CARRIED} />}
              rightTitle={t('restoreFlow.approve.sealed')}
              right={<ItemList keys={request.sealed?.length ? request.sealed : ADOPTION_NOT_CARRIED} />}
            />
          </section>
        ) : (
          <TwoLists
            leftTitle={t('myPersonal.restore.willRestore')}
            left={<ItemList keys={RESTORED_ITEMS} />}
            rightTitle={t('myPersonal.restore.adminResetup')}
            right={<ItemList keys={ADMIN_RESETUP_ITEMS} />}
          />
        )}

        {shared && request.members && (
          <section className="space-y-1">
            <h4 className="text-sm font-medium">{t('restoreFlow.shared.members')}</h4>
            <ul className="space-y-0.5 text-sm">
              {request.members.map((m) => (
                <li key={m.user_id}>{m.name || m.user_id} · {t(`restoreFlow.level.${m.access_level}`)}</li>
              ))}
            </ul>
            {request.routes && (
              <p className="text-xs text-muted-foreground">{t('restoreFlow.approve.routes', { count: request.routes.length })}</p>
            )}
          </section>
        )}

        {request.failure && !reply && (
          <div className="space-y-1">
            <p className="text-sm font-medium">{t('restoreFlow.approve.failed')}</p>
            <RestoreErrorNotice error={request.failure} />
          </div>
        )}

        {/* Board "Restore": one note per approve state (second admin / sole admin / cooling). */}
        {!reply && !denied && (
          request.owner_unverified ? (
            <p className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/5 px-3 py-2 text-sm">
              <Info className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
              {t(mine ? 'restoreFlow.approve.ownerUnverifiedMine' : 'restoreFlow.approve.ownerUnverifiedOther')}
            </p>
          ) : state === 'second_admin' ? (
            <p className="text-sm">{t('restoreFlow.approve.secondAdmin')}</p>
          ) : (
            <p className="rounded-lg border border-warning/30 bg-warning/5 px-3 py-2 text-sm">
              {state === 'ready' && t(adoption ? 'restoreFlow.approve.readyAdopt' : 'restoreFlow.approve.ready')}
              {state === 'sole_admin_self' && t('restoreFlow.approve.soleAdmin')}
              {state === 'cooling' && t('restoreFlow.approve.cooling')}
              {state === 'cooling' && coolingMs != null && (
                <> {t('restoreFlow.approve.coolingUntil', {
                  date: intl.formatDate(coolingMs, { dateStyle: 'medium', timeStyle: 'short' }),
                })}</>
              )}
            </p>
          )
        )}

        <RestoreErrorNotice error={error} />

        {reply && (
          <section role="status" aria-label={t('restoreFlow.result.title')} className="space-y-3 rounded-lg border border-surface-border p-3">
            <p className="flex items-start gap-2 text-sm font-medium">
              <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
              {t(adoption ? 'restoreFlow.result.adopted' : 'restoreFlow.result.restored', { name })}
            </p>
            <ul className="list-disc space-y-1 ps-5 text-sm">
              {reply.self_approved && <li>{t('restoreFlow.result.selfApproved')}</li>}
              {count(reply.work_paused) > 0 && <li>{t('restoreFlow.result.workPaused')}</li>}
              {count(reply.routes_need_provisioning) > 0 && <li>{t('restoreFlow.result.routesNeedProvisioning')}</li>}
              {shared && reply.members && <li>{t('restoreFlow.result.members', { count: reply.members.length })}</li>}
              {shared && count(reply.routes_reapproved) > 0 && (
                <li>{t('restoreFlow.result.routesReapproved', { count: count(reply.routes_reapproved) })}</li>
              )}
              {shared && count(reply.members_notified) > 0 && (
                <li>{t('restoreFlow.result.membersNotified', { count: count(reply.members_notified) })}</li>
              )}
            </ul>
            {shared && reply.members_dropped && reply.members_dropped.length > 0 && (
              <div className="space-y-1">
                <h4 className="text-sm font-medium">{t('restoreFlow.result.membersDropped')}</h4>
                <ul className="list-disc space-y-0.5 ps-5 text-sm">
                  {reply.members_dropped.map((m) => {
                    const id = typeof m === 'string' ? m : m.user_id;
                    const label = typeof m === 'string' ? m : `${m.name || m.user_id}${m.reason ? ` · ${m.reason}` : ''}`;
                    return <li key={id}>{label}</li>;
                  })}
                </ul>
              </div>
            )}
            <RegrantList
              employeeId={request.employee_id}
              incarnation={reply.incarnation ?? request.incarnation}
              items={reply.label_state?.regrant ?? []}
              skipped={reply.label_state?.skipped ?? []}
              listed={reply.label_state?.listed ?? []}
              withToken={withToken}
            />
          </section>
        )}
        {denied && !reply && <p role="status" className="text-sm text-muted-foreground">{t('restoreFlow.approve.closed')}</p>}
      </CardContent>

      {!reply && !denied && (
        <CardFooter className="flex flex-wrap justify-end gap-2">
          {mine && (
            <Button variant="ghost" disabled={busy} onClick={() => void withdraw()}>{t('restoreFlow.approve.withdraw')}</Button>
          )}
          <Button variant="outline" disabled={busy} onClick={() => void deny()}>{t('restoreFlow.approve.deny')}</Button>
          <Button
            variant="brand"
            disabled={busy || state === 'second_admin' || state === 'cooling'}
            onClick={() => void approve()}
          >
            {/* A sole Admin's own request (cooling or not) is always a self-approval. */}
            {busy ? t('restoreFlow.common.sending') : t(`restoreFlow.approve.${
              state === 'sole_admin_self' || state === 'cooling' ? 'selfApprove' : 'approve'}${needsVerify ? 'Verify' : ''}`)}
          </Button>
        </CardFooter>
      )}
    </Card>
  );
}
