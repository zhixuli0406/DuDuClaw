import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link } from 'react-router';
import { useIntl } from 'react-intl';
import { AlertTriangle, ArchiveRestore, Plus, Trash2, UserPlus } from 'lucide-react';
import {
  api,
  type IsolationReason,
  type ProtectedEmployee,
  type ProtectedEmployeeList,
  type ProtectedEmployeeRequest,
  type ProtectedTombstoneResult,
} from '@/lib/api';
import {
  daysUntil,
  rootStateOf,
  toMillis,
  type IsolationEmployeeList,
  type QuarantineRow,
  type RestoreRequestRow,
} from '@/lib/isolation-restore-api';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useStepUpToken } from '@/components/step-up/useStepUpToken';
import { ADMIN_RESTORE_PATH, QuarantineRowDialogs } from './restore/AdminRestorePage';
import { rowActions, rowKey, type RowAction } from './restore/QuarantineTable';
import { NeedsVerifyProvider, RestorePeopleProvider } from './restore/restore-ui';
import { makeWithToken } from './restore/with-token';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Empty,
  Input,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/mds';
import { StepUpDialog, useStepUp } from './step-up';
import {
  LABEL_PATTERN,
  Refusal,
  TableBox,
  formatWhen,
  refusalMessage,
  usePersonName,
  useProtectedPage,
  useProviderName,
  useT,
} from './shared';

const APPROVAL_CENTER = '/app/system/migration';

/** `isolation.employee_list` also carries the F6 restore lists (`restores`). */
type EmployeeListWithRestores = ProtectedEmployeeList & Pick<IsolationEmployeeList, 'restores'>;

type RequestView = 'toPrepare' | 'awaitingApproval' | 'applying' | 'failed' | 'rejected' | 'expired';

function requestView(request: ProtectedEmployeeRequest): RequestView | null {
  switch (request.status) {
    case 'pending':
      return request.stage === 'requested' ? 'toPrepare' : 'awaitingApproval';
    case 'applying':
      return 'applying';
    case 'failed':
      return 'failed';
    case 'rejected':
      return 'rejected';
    case 'expired':
      return 'expired';
    default:
      // Applied requests are listed as employees.
      return null;
  }
}

export function EmployeesTab() {
  const t = useT();
  const intl = useIntl();
  const connection = useConnectionStore((s) => s.state);
  const stepUp = useStepUp();
  const record = stepUp.record;
  const personName = usePersonName();
  const providerName = useProviderName();
  const { goTo, people } = useProtectedPage();
  const readiness = useSystemStore((s) => s.status?.isolation_readiness);
  const agents = useAgentsStore((s) => s.agents);
  const [data, setData] = useState<EmployeeListWithRestores | null>(null);
  // Retired rows open the restore screen's own dialogs (part B).
  const [restoreAction, setRestoreAction] = useState<{ row: QuarantineRow; action: RowAction } | null>(null);
  const { ensure, dialog: restoreStepUp } = useStepUpToken();
  const withToken = useMemo(() => makeWithToken(ensure), [ensure]);
  const names = useMemo(
    () => new Map([...people].map(([id, person]) => [id, person.name || person.email])),
    [people],
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [preparing, setPreparing] = useState<ProtectedEmployeeRequest | null>(null);
  const [purging, setPurging] = useState(false);
  const [creating, setCreating] = useState(false);
  const [newLabel, setNewLabel] = useState('');
  const [newName, setNewName] = useState('');
  const [retiring, setRetiring] = useState<ProtectedEmployee | null>(null);
  const [tombstoned, setTombstoned] = useState<{ name: string; result: ProtectedTombstoneResult } | null>(null);
  const generation = useRef(0);

  const load = useCallback(async () => {
    const current = ++generation.current;
    try {
      const list = await api.protectedMode.employeeList();
      if (current !== generation.current) return;
      setData(list);
      setLoadError(null);
      record(list);
    } catch (e) {
      if (current !== generation.current) return;
      setData(null);
      setLoadError(refusalMessage(e));
    }
  }, [record]);

  useEffect(() => {
    if (connection === 'authenticated') void load();
    return () => { generation.current += 1; };
  }, [connection, load]);

  const displayName = useCallback(
    (label: string) => agents.find((agent) => agent.name === label)?.display_name || label,
    [agents],
  );

  const notReady = useMemo(
    () => new Map((readiness?.employees ?? []).filter((e) => !e.ready).map((e) => [e.label, e.reasons])),
    [readiness],
  );

  const requests = (data?.requests ?? [])
    .map((request) => ({ request, view: requestView(request) }))
    .filter((row): row is { request: ProtectedEmployeeRequest; view: RequestView } => row.view !== null);
  const closedRequests = requests.filter((row) => row.view === 'rejected' || row.view === 'expired').length;
  const employees = data?.employees ?? [];
  const restoreRequests: RestoreRequestRow[] = data?.restores?.requests ?? [];
  const live = new Set(employees.map((employee) => employee.employee_id));
  const retired: QuarantineRow[] = (data?.restores?.quarantine ?? []).filter((row) => !live.has(row.employee_id));
  const openCount = requests.filter((row) => row.view === 'toPrepare' || row.view === 'awaitingApproval').length
    + restoreRequests.length;

  const retiredStatus = (row: QuarantineRow): string => {
    if (row.purge?.purged_at != null) return t('employees.status.purged');
    const root = rootStateOf(row);
    if (root === 'root_gone') return t('employees.status.retiredAdopt');
    if (root !== 'restorable') return t('employees.status.retired');
    const days = daysUntil(row.restore_deadline ?? row.purge?.purge_after);
    return days == null ? t('employees.status.retired') : t('employees.status.retiredDays', { days });
  };

  const reasonText = (reason: IsolationReason): string => {
    switch (reason.code) {
      case 'connector_setup_required':
        return reason.provider ? t('attention.connector', { provider: providerName(reason.provider) }) : t('attention.connectorAny');
      case 'personal_binding_conflict':
        return t('attention.ownerNotSignedIn');
      case 'revoked':
        return t('attention.revoked');
      default:
        return t('attention.other');
    }
  };

  const attention = [...notReady.entries()];

  return (
    <div className="space-y-6">
      {attention.length > 0 && (
        <section
          aria-labelledby="protected-attention-title"
          className="rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm"
        >
          <div className="flex items-start gap-3">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
            <div className="min-w-0 flex-1 space-y-2">
              <h2 id="protected-attention-title" className="font-medium">
                {t('attention.title', { count: attention.length })}
              </h2>
              <ul className="space-y-2">
                {attention.map(([label, reasons]) => {
                  const setup = reasons.find((reason) => reason.code === 'connector_setup_required');
                  const revoked = reasons.some((reason) => reason.code === 'revoked');
                  return (
                    <li key={label} className="flex flex-wrap items-center gap-x-3 gap-y-1">
                      <span className="font-medium">{displayName(label)}</span>
                      <span className="text-muted-foreground">
                        {reasons.length > 0 ? [...new Set(reasons.map(reasonText))].join(' ') : t('attention.other')}
                      </span>
                      {setup && (
                        <Button size="sm" variant="outline" onClick={() => goTo('connectors', { employee: label, provider: setup.provider })}>
                          {t('attention.finishSetup')}
                        </Button>
                      )}
                      {!setup && revoked && (
                        <Button size="sm" variant="outline" onClick={() => goTo('connectors')}>
                          {t('attention.openConnectors')}
                        </Button>
                      )}
                    </li>
                  );
                })}
              </ul>
            </div>
          </div>
        </section>
      )}

      <Refusal message={loadError} />
      {notice && <p role="status" className="rounded-lg border border-success/30 bg-success/10 px-3 py-2 text-sm">{notice}</p>}
      {tombstoned && <TombstoneOutcome name={tombstoned.name} result={tombstoned.result} onDismiss={() => setTombstoned(null)} />}
      {!data && !loadError && <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>}

      {data && (
        <>
          <Card>
            <CardHeader>
              <CardTitle>
                <h2>
                  {t('requests.title')}
                  {openCount > 0 && (
                    <span className="ml-1 text-sm font-normal text-muted-foreground">{t('requests.openCount', { count: openCount })}</span>
                  )}
                </h2>
              </CardTitle>
              <CardDescription>{t('requests.description')}</CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              {requests.length === 0 && restoreRequests.length === 0 ? (
                <Empty variant="dashed" icon={UserPlus} title={t('requests.empty')} description={t('requests.emptyHint')} />
              ) : (
                <TableBox label={t('requests.title')}>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead scope="col">{t('requests.col.requester')}</TableHead>
                        <TableHead scope="col">{t('requests.col.name')}</TableHead>
                        <TableHead scope="col">{t('requests.col.when')}</TableHead>
                        <TableHead scope="col">{t('requests.col.status')}</TableHead>
                        <TableHead scope="col">{t('col.action')}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {requests.map(({ request, view }) => (
                        <TableRow key={request.request_id}>
                          <TableCell>{personName(request.owner_user_id)}</TableCell>
                          <TableCell>
                            <span dir="auto" className="[unicode-bidi:isolate]">{request.display_name}</span>
                            <span className="ml-2 text-xs text-muted-foreground">{request.label}</span>
                          </TableCell>
                          <TableCell>{formatWhen(request.created_at, intl.locale)}</TableCell>
                          <TableCell>
                            <Badge variant={view === 'toPrepare' ? 'default' : view === 'rejected' || view === 'expired' || view === 'failed' ? 'ghost' : 'secondary'}>
                              {t(`requests.status.${view}`)}
                            </Badge>
                          </TableCell>
                          <TableCell>
                            {view === 'toPrepare' && (
                              <Button size="sm" variant="brand" onClick={() => setPreparing(request)}>
                                {t('requests.prepare')}
                              </Button>
                            )}
                            {view === 'awaitingApproval' && (
                              <Link to={APPROVAL_CENTER} className="text-sm underline">{t('requests.openApprovals')}</Link>
                            )}
                          </TableCell>
                        </TableRow>
                      ))}
                      {/* F6 restore / take-over requests: decided on the restore screen. */}
                      {restoreRequests.map((request) => {
                        const adoption = request.kind === 'adoption';
                        return (
                          <TableRow key={`restore:${request.request_id}`}>
                            <TableCell>{personName(request.owner_user_id ?? request.filed_by)}</TableCell>
                            <TableCell>
                              <span dir="auto" className="[unicode-bidi:isolate]">
                                {t(adoption ? 'requests.adoptName' : 'requests.restoreName', { name: displayName(request.label) })}
                              </span>
                            </TableCell>
                            <TableCell>
                              {toMillis(request.created_at) != null
                                ? intl.formatDate(toMillis(request.created_at)!, { dateStyle: 'medium', timeStyle: 'short' }) : ''}
                            </TableCell>
                            <TableCell>
                              <Badge variant={request.owner_unverified ? 'destructive' : 'secondary'}>
                                {t(request.owner_unverified ? 'requests.status.ownerUnverified' : 'requests.status.secondAdmin')}
                              </Badge>
                            </TableCell>
                            <TableCell>
                              <Link to={ADMIN_RESTORE_PATH} className="text-sm underline">
                                {t(adoption ? 'requests.decideAdoption' : 'requests.decideRestore')}
                              </Link>
                            </TableCell>
                          </TableRow>
                        );
                      })}
                    </TableBody>
                  </Table>
                </TableBox>
              )}
              <div className="flex flex-wrap items-center gap-2">
                <Button variant="outline" size="sm" disabled={closedRequests === 0} onClick={() => setPurging(true)}>
                  <Trash2 aria-hidden="true" />
                  {t('requests.purge')}
                </Button>
                {closedRequests === 0 && <span className="text-xs text-muted-foreground">{t('requests.purgeNone')}</span>}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle><h2>{t('employees.title')}</h2></CardTitle>
              <CardDescription>{t('employees.description')}</CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="flex flex-wrap items-center justify-end gap-2">
                <Link
                  to={ADMIN_RESTORE_PATH}
                  className="inline-flex h-7 items-center gap-1 rounded-lg px-2.5 text-[0.8rem] font-medium underline-offset-4 hover:underline"
                >
                  <ArchiveRestore className="size-4" aria-hidden="true" />
                  {t('employees.openRestore')}
                </Link>
                <Button variant="brand" size="sm" onClick={() => setCreating(true)}>
                  <Plus aria-hidden="true" />
                  {t('employees.addShared')}
                </Button>
              </div>
              {employees.length === 0 && retired.length === 0 ? (
                <Empty variant="dashed" title={t('employees.empty')} description={t('employees.emptyHint')} />
              ) : (
                <TableBox label={t('employees.title')}>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead scope="col">{t('employees.col.name')}</TableHead>
                        <TableHead scope="col">{t('employees.col.type')}</TableHead>
                        <TableHead scope="col">{t('employees.col.owner')}</TableHead>
                        <TableHead scope="col">{t('employees.col.status')}</TableHead>
                        <TableHead scope="col">{t('col.action')}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {employees.map((employee) => {
                        const personal = employee.binding.type === 'personal';
                        const status = employee.disabled ? 'retired' : notReady.has(employee.label) ? 'attention' : 'running';
                        return (
                          <TableRow key={employee.employee_id}>
                            <TableCell>
                              <span dir="auto" className="[unicode-bidi:isolate]">{displayName(employee.label)}</span>
                              {displayName(employee.label) !== employee.label && (
                                <span className="ml-2 text-xs text-muted-foreground">{employee.label}</span>
                              )}
                            </TableCell>
                            <TableCell>
                              <Badge variant="outline">{t(personal ? 'type.personal' : 'type.shared')}</Badge>
                            </TableCell>
                            <TableCell>
                              {employee.binding.type === 'personal' ? personName(employee.binding.owner_user_id) : <><span aria-hidden="true">—</span><span className="sr-only">{t('none')}</span></>}
                            </TableCell>
                            <TableCell>
                              <Badge variant={status === 'running' ? 'secondary' : status === 'attention' ? 'destructive' : 'ghost'}>
                                {t(`employees.status.${status}`)}
                              </Badge>
                            </TableCell>
                            <TableCell>
                              {personal && !employee.disabled ? (
                                <Button
                                  size="sm"
                                  variant="destructive"
                                  aria-label={t('employees.retireNamed', { name: displayName(employee.label) })}
                                  onClick={() => setRetiring(employee)}
                                >
                                  {t('employees.retire')}
                                </Button>
                              ) : (
                                <span className="text-xs text-muted-foreground">{employee.disabled ? '' : t('employees.sharedRetireLater')}</span>
                              )}
                            </TableCell>
                          </TableRow>
                        );
                      })}
                      {/* Retired (tombstoned) employees: same actions as the restore screen. */}
                      {retired.map((row) => {
                        const actions = rowActions(row);
                        const adopt = rootStateOf(row) === 'root_gone';
                        const name = displayName(row.label);
                        return (
                          <TableRow key={`retired:${rowKey(row)}`}>
                            <TableCell>
                              <span dir="auto" className="text-muted-foreground [unicode-bidi:isolate]">{name}</span>
                            </TableCell>
                            <TableCell>
                              <Badge variant="outline">{t(row.kind === 'personal' ? 'type.personal' : 'type.shared')}</Badge>
                            </TableCell>
                            <TableCell>
                              {row.kind === 'personal' ? personName(row.owner_user_id) : <><span aria-hidden="true">—</span><span className="sr-only">{t('none')}</span></>}
                            </TableCell>
                            <TableCell>
                              <Badge variant="ghost">{retiredStatus(row)}</Badge>
                            </TableCell>
                            <TableCell>
                              <div className="flex flex-wrap items-center gap-1.5">
                                {actions.map((action) => {
                                  const label = intl.formatMessage({ id: `restoreFlow.action.${action}${adopt && (action === 'shared_restore' || action === 'file_for_owner') ? 'Adopt' : ''}` });
                                  return (
                                    <Button
                                      key={action}
                                      size="sm"
                                      variant={action === 'retire' || action === 'shared_restore' || action === 'file_for_owner' ? 'outline' : 'destructive'}
                                      aria-label={intl.formatMessage({ id: 'restoreFlow.action.forRow' }, { action: label, name })}
                                      onClick={() => setRestoreAction({ row, action })}
                                    >
                                      {label}
                                    </Button>
                                  );
                                })}
                                <Link to={ADMIN_RESTORE_PATH} className="text-sm underline" aria-label={t('employees.viewRetiredNamed', { name })}>
                                  {t('employees.viewRetired')}
                                </Link>
                              </div>
                            </TableCell>
                          </TableRow>
                        );
                      })}
                    </TableBody>
                  </Table>
                </TableBox>
              )}
            </CardContent>
          </Card>
        </>
      )}

      <RestorePeopleProvider value={names}>
        <NeedsVerifyProvider value={!stepUp.status.valid}>
          <QuarantineRowDialogs
            active={restoreAction}
            onClose={() => setRestoreAction(null)}
            withToken={withToken}
            onDone={() => void load()}
          />
        </NeedsVerifyProvider>
      </RestorePeopleProvider>
      {restoreStepUp}

      {preparing && (
        <StepUpDialog
          open
          idPrefix="prepare"
          onOpenChange={(open) => { if (!open) setPreparing(null); }}
          title={t('prepare.title')}
          description={t('prepare.description')}
          confirmLabel={t('prepare.confirm')}
          verifyConfirmLabel={t('prepare.verifyConfirm')}
          onConfirm={async (csrf) => {
            await api.protectedMode.propose(
              { label: preparing.label, binding_kind: 'personal', request_id: preparing.request_id },
              csrf,
            );
            setNotice(t('prepare.done', { name: preparing.display_name }));
            await load();
          }}
        >
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
            <dt className="text-muted-foreground">{t('prepare.employee')}</dt>
            <dd><span dir="auto" className="[unicode-bidi:isolate]">{preparing.display_name}</span> <span className="text-xs text-muted-foreground">{preparing.label}</span></dd>
            <dt className="text-muted-foreground">{t('prepare.owner')}</dt>
            <dd>{personName(preparing.owner_user_id)}</dd>
            <dt className="text-muted-foreground">{t('prepare.type')}</dt>
            <dd>{t('type.personal')}</dd>
          </dl>
        </StepUpDialog>
      )}

      {purging && (
        <StepUpDialog
          open
          idPrefix="purge"
          onOpenChange={setPurging}
          title={t('purge.title')}
          description={t('purge.description', { count: closedRequests })}
          confirmLabel={t('purge.confirm')}
          verifyConfirmLabel={t('purge.verifyConfirm')}
          onConfirm={async (csrf) => {
            const result = await api.protectedMode.purgeRequests(csrf);
            setNotice(t('purge.done', { count: result.purged }));
            await load();
          }}
        />
      )}

      {creating && (
        <StepUpDialog
          open
          idPrefix="shared"
          onOpenChange={(open) => { setCreating(open); if (!open) { setNewLabel(''); setNewName(''); } }}
          title={t('shared.title')}
          description={t('shared.description')}
          confirmLabel={t('shared.confirm')}
          verifyConfirmLabel={t('shared.verifyConfirm')}
          disabled={!LABEL_PATTERN.test(newLabel) || newName.trim().length === 0 || newName.trim().length > 64}
          onConfirm={async (csrf) => {
            await api.protectedMode.propose(
              { label: newLabel, display_name: newName.trim(), binding_kind: 'shared' },
              csrf,
            );
            setNotice(t('shared.done', { name: newName.trim() }));
            setNewLabel('');
            setNewName('');
            await load();
          }}
        >
          <div className="space-y-3">
            <div className="space-y-1">
              <label htmlFor="shared-display" className="block text-sm font-medium">{t('shared.displayName')}</label>
              <Input id="shared-display" maxLength={64} value={newName} onChange={(e) => setNewName(e.target.value)} />
            </div>
            <div className="space-y-1">
              <label htmlFor="shared-label" className="block text-sm font-medium">{t('shared.label')}</label>
              <Input
                id="shared-label"
                maxLength={64}
                autoCapitalize="none"
                spellCheck={false}
                aria-describedby="shared-label-hint"
                aria-invalid={newLabel.length > 0 && !LABEL_PATTERN.test(newLabel)}
                value={newLabel}
                onChange={(e) => setNewLabel(e.target.value.trim().toLowerCase())}
              />
              <p id="shared-label-hint" className="text-xs text-muted-foreground">{t('shared.labelHint')}</p>
            </div>
          </div>
        </StepUpDialog>
      )}

      {retiring && (
        <StepUpDialog
          open
          destructive
          idPrefix="retire"
          onOpenChange={(open) => { if (!open) setRetiring(null); }}
          title={t('retire.title', { name: displayName(retiring.label) })}
          confirmLabel={t('retire.confirm')}
          verifyConfirmLabel={t('retire.verifyConfirm')}
          onConfirm={async (csrf) => {
            const result = await api.protectedMode.tombstone(retiring.label, retiring.employee_id, csrf);
            setTombstoned({ name: displayName(retiring.label), result });
            await load();
          }}
        >
          {/* Board "Main": what happens, then a note on restorability and the reserved name. */}
          <p className="text-sm leading-relaxed">{t('retire.quarantine')}</p>
          <div className="space-y-1 rounded-lg bg-muted/60 px-3 py-2 text-sm">
            <p>{t('retire.restorable')}</p>
            <p>{t('retire.labelReserved', { label: retiring.label })}</p>
          </div>
          <p className="text-sm text-muted-foreground">{t('retire.connectors')}</p>
        </StepUpDialog>
      )}
    </div>
  );
}

function TombstoneOutcome({ name, result, onDismiss }: { name: string; result: ProtectedTombstoneResult; onDismiss: () => void }) {
  const t = useT();
  const seal = result.seal === 'sealed' ? 'sealed' : result.seal === 'oversize' ? 'oversize' : 'incomplete';
  return (
    <div role="status" className="space-y-1 rounded-lg border border-surface-border bg-surface px-4 py-3 text-sm">
      <p className="font-medium">{t('retire.done', { name })}</p>
      <p className="text-muted-foreground">{t(`retire.seal.${seal}`)}</p>
      {result.routes_revoked > 0 && <p className="text-muted-foreground">{t('retire.routesRevoked', { count: result.routes_revoked })}</p>}
      {(result.pending_fences.length > 0 || !result.runs_stopped) && (
        <p className="text-warning">{t('retire.pending')}</p>
      )}
      <Button size="sm" variant="ghost" onClick={onDismiss}>{t('dismiss')}</Button>
    </div>
  );
}
