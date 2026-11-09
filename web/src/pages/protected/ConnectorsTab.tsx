import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link } from 'react-router';
import { Import, Plug, Plus } from 'lucide-react';
import {
  api,
  type ProtectedConnectorRoute,
  type ProtectedEmployee,
  type ProtectedImportResult,
} from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Empty,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/mds';
import { ConnectorWizard } from './ConnectorWizard';
import { StepUpDialog, useStepUp } from './step-up';
import {
  Refusal,
  TableBox,
  kindKey,
  refusalMessage,
  usePersonName,
  useProviderName,
  useT,
  type WizardPreset,
} from './shared';

const APPROVAL_CENTER = '/app/system/migration';
/** `isolation.connector_list` pages; a bound so a broken cursor cannot loop. */
const MAX_PAGES = 50;

type RowStatus = 'available' | 'setupRequired' | 'revoked';
interface Row {
  key: string;
  provider: string;
  account: string | null;
  kind: string | null;
  status: RowStatus;
  route?: ProtectedConnectorRoute;
  /** For a setup-required row: the employee that needs it. */
  employee?: string;
}
interface Group {
  key: string;
  /** `null` for shared employees. */
  owner: string | null;
  employees: string[];
  rows: Row[];
}

function groupKey(binding: ProtectedConnectorRoute['binding'] | ProtectedEmployee['binding']): string {
  return binding.type === 'personal' ? `personal:${binding.owner_user_id}` : 'shared';
}

export function ConnectorsTab({ wizardPreset, onWizardPresetUsed }: { wizardPreset?: WizardPreset | null; onWizardPresetUsed?: () => void }) {
  const t = useT();
  const connection = useConnectionStore((s) => s.state);
  const { record } = useStepUp();
  const personName = usePersonName();
  const providerName = useProviderName();
  const readiness = useSystemStore((s) => s.status?.isolation_readiness);
  const [routes, setRoutes] = useState<ProtectedConnectorRoute[] | null>(null);
  const [employees, setEmployees] = useState<ProtectedEmployee[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<ProtectedConnectorRoute | null>(null);
  const [importing, setImporting] = useState(false);
  const [imported, setImported] = useState<ProtectedImportResult | null>(null);
  const [wizard, setWizard] = useState<{ open: boolean; preset?: WizardPreset }>({ open: false });
  const generation = useRef(0);

  const load = useCallback(async () => {
    const current = ++generation.current;
    try {
      const all: ProtectedConnectorRoute[] = [];
      let after: string | undefined;
      for (let page = 0; page < MAX_PAGES; page += 1) {
        const list = await api.protectedMode.connectorList(after);
        if (current !== generation.current) return;
        if (page === 0) record(list);
        all.push(...list.routes);
        if (!list.next || list.next === after) break;
        after = list.next;
      }
      const staff = await api.protectedMode.employeeList();
      if (current !== generation.current) return;
      setRoutes(all);
      setEmployees(staff.employees);
      setLoadError(null);
    } catch (e) {
      if (current !== generation.current) return;
      setRoutes(null);
      setLoadError(refusalMessage(e));
    }
  }, [record]);

  useEffect(() => {
    if (connection === 'authenticated') void load();
    return () => { generation.current += 1; };
  }, [connection, load]);

  // A preset from the 「AI 員工」 tab's 完成設定 opens the wizard once.
  useEffect(() => {
    if (!wizardPreset) return;
    setWizard({ open: true, preset: wizardPreset });
    onWizardPresetUsed?.();
  }, [wizardPreset, onWizardPresetUsed]);

  const groups = useMemo<Group[]>(() => {
    const byKey = new Map<string, Group>();
    const ensure = (key: string, owner: string | null) => {
      let group = byKey.get(key);
      if (!group) {
        group = { key, owner, employees: [], rows: [] };
        byKey.set(key, group);
      }
      return group;
    };
    for (const employee of employees) {
      if (employee.disabled) continue;
      const owner = employee.binding.type === 'personal' ? employee.binding.owner_user_id : null;
      ensure(groupKey(employee.binding), owner).employees.push(employee.label);
    }
    for (const route of routes ?? []) {
      const owner = route.binding.type === 'personal' ? route.binding.owner_user_id : null;
      ensure(groupKey(route.binding), owner).rows.push({
        key: route.account,
        provider: route.provider,
        account: route.label,
        kind: route.kind,
        status: route.active ? 'available' : 'revoked',
        route,
      });
    }
    // Employees whose tools still need a connector (readiness evidence).
    const labelGroup = new Map(employees.map((e) => [e.label, groupKey(e.binding)]));
    for (const entry of readiness?.employees ?? []) {
      const key = labelGroup.get(entry.label);
      if (!key) continue;
      const group = byKey.get(key);
      if (!group) continue;
      for (const reason of entry.reasons) {
        if (reason.code !== 'connector_setup_required' || !reason.provider) continue;
        const provider = reason.provider;
        if (group.rows.some((row) => row.provider === provider && row.status !== 'revoked')) continue;
        group.rows.push({ key: `setup:${entry.label}:${provider}`, provider, account: null, kind: null, status: 'setupRequired', employee: entry.label });
      }
    }
    return [...byKey.values()]
      .filter((group) => group.rows.length > 0 || group.employees.length > 0)
      .sort((a, b) => (a.owner === null ? -1 : b.owner === null ? 1 : personName(a.owner).localeCompare(personName(b.owner))));
  }, [employees, routes, readiness, personName]);

  const totalRows = groups.reduce((sum, group) => sum + group.rows.length, 0);

  const statusBadge = (status: RowStatus) => (
    <Badge variant={status === 'available' ? 'secondary' : status === 'setupRequired' ? 'destructive' : 'ghost'}>
      {t(`connectors.status.${status}`)}
    </Badge>
  );

  return (
    <div className="space-y-6">
      <Refusal message={loadError} />
      {notice && <p role="status" className="rounded-lg border border-success/30 bg-success/10 px-3 py-2 text-sm">{notice}</p>}

      {imported && (
        <section role="status" aria-labelledby="import-result-title" className="space-y-2 rounded-lg border border-info/30 bg-info/10 px-4 py-3 text-sm">
          <h2 id="import-result-title" className="font-medium">{t('import.resultTitle', { count: imported.staged })}</h2>
          {imported.unchanged > 0 && <p>{t('import.unchanged', { count: imported.unchanged })}</p>}
          {imported.closed > 0 && <p>{t('import.closed', { count: imported.closed })}</p>}
          {imported.unproven_personal > 0 && <p>{t('import.unproven', { count: imported.unproven_personal })}</p>}
          <p className="text-muted-foreground">{t('import.next')}</p>
          <div className="flex flex-wrap gap-2">
            <Link to={APPROVAL_CENTER} className="inline-flex h-7 items-center rounded-lg bg-brand px-2.5 text-[0.8rem] font-medium text-brand-foreground hover:bg-brand/90">
              {t('import.openApprovals')}
            </Link>
            <Button size="sm" variant="ghost" onClick={() => setImported(null)}>{t('dismiss')}</Button>
          </div>
        </section>
      )}

      <Card>
        <CardHeader>
          <CardTitle><h2>{t('connectors.title')}</h2></CardTitle>
          <CardDescription>{t('connectors.description')}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="outline" size="sm" onClick={() => setImporting(true)} disabled={routes === null}>
              <Import aria-hidden="true" />
              {t('import.button')}
            </Button>
            <Button variant="brand" size="sm" onClick={() => setWizard({ open: true })} disabled={routes === null}>
              <Plus aria-hidden="true" />
              {t('connectors.add')}
            </Button>
          </div>

          {routes === null && !loadError && <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>}
          {routes !== null && totalRows === 0 && (
            <Empty variant="dashed" icon={Plug} title={t('connectors.empty')} description={t('connectors.emptyHint')} />
          )}

          {routes !== null && groups.filter((group) => group.rows.length > 0).map((group) => {
            const heading = group.owner === null ? t('group.shared') : t('group.personal', { owner: personName(group.owner) });
            return (
              <div key={group.key} className="space-y-2">
                <div>
                  <h3 className="text-sm font-medium">{heading}</h3>
                  {group.employees.length > 0 && (
                    <p className="text-xs text-muted-foreground">{t('group.employees', { names: group.employees.join('、') })}</p>
                  )}
                </div>
                <TableBox label={heading}>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead scope="col">{t('connectors.col.service')}</TableHead>
                        <TableHead scope="col">{t('connectors.col.source')}</TableHead>
                        <TableHead scope="col">{t('connectors.col.status')}</TableHead>
                        <TableHead scope="col">{t('col.action')}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {group.rows.map((row) => (
                        <TableRow key={row.key}>
                          <TableCell>
                            <span className="font-medium">{providerName(row.provider)}</span>
                            {' · '}
                            {row.account ? (
                              <span dir="auto" className="[unicode-bidi:isolate]">{row.account}</span>
                            ) : (
                              <span className="text-muted-foreground">{t('connectors.forEmployee', { name: row.employee ?? '' })}</span>
                            )}
                          </TableCell>
                          <TableCell className="text-muted-foreground">
                            {row.kind ? t(`kind.${kindKey(row.kind)}`) : t('connectors.notSetUp')}
                          </TableCell>
                          <TableCell>{statusBadge(row.status)}</TableCell>
                          <TableCell>
                            {row.status === 'available' && row.route && (
                              <Button size="sm" variant="destructive" aria-label={t('connectors.revokeNamed', { name: `${providerName(row.provider)} · ${row.account ?? ''}` })} onClick={() => setRevoking(row.route!)}>
                                {t('connectors.revoke')}
                              </Button>
                            )}
                            {row.status === 'setupRequired' && (
                              <Button size="sm" variant="brand" onClick={() => setWizard({ open: true, preset: { employee: row.employee, provider: row.provider } })}>
                                {t('connectors.finishSetup')}
                              </Button>
                            )}
                            {row.status === 'revoked' && (
                              <Button
                                size="sm"
                                variant="outline"
                                onClick={() => setWizard({
                                  open: true,
                                  preset: {
                                    provider: row.provider,
                                    label: row.account ?? undefined,
                                    employee: group.employees.length === 1 ? group.employees[0] : undefined,
                                  },
                                })}
                              >
                                {t('connectors.recreate')}
                              </Button>
                            )}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </TableBox>
              </div>
            );
          })}
          <p className="text-xs text-muted-foreground">{t('connectors.noSecrets')}</p>
        </CardContent>
      </Card>

      <ConnectorWizard
        open={wizard.open}
        preset={wizard.preset}
        employees={employees}
        onOpenChange={(open) => setWizard((current) => ({ ...current, open }))}
        onCreated={async (summary) => {
          setNotice(t('wizard.done', { name: summary }));
          await load();
        }}
      />

      {revoking && (
        <StepUpDialog
          open
          destructive
          idPrefix="revoke"
          onOpenChange={(open) => { if (!open) setRevoking(null); }}
          title={t('revoke.title', { name: `${providerName(revoking.provider)} · ${revoking.label}` })}
          confirmLabel={t('revoke.confirm')}
          verifyConfirmLabel={t('revoke.verifyConfirm')}
          onConfirm={async (csrf) => {
            await api.protectedMode.revoke(revoking.account, csrf);
            setNotice(t('revoke.done', { name: `${providerName(revoking.provider)} · ${revoking.label}` }));
            await load();
          }}
        >
          <ul className="list-disc space-y-1.5 pl-5 text-sm">
            <li>{t('revoke.inFlight')}</li>
            <li>{t('revoke.permanent')}</li>
          </ul>
        </StepUpDialog>
      )}

      {importing && (
        <StepUpDialog
          open
          idPrefix="import"
          onOpenChange={setImporting}
          title={t('import.title')}
          description={t('import.description')}
          confirmLabel={t('import.confirm')}
          verifyConfirmLabel={t('import.verifyConfirm')}
          onConfirm={async (csrf) => {
            const result = await api.protectedMode.importLegacy(csrf);
            setImported(result);
            await load();
          }}
        >
          <ul className="list-disc space-y-1.5 pl-5 text-sm">
            <li>{t('import.proposalsOnly')}</li>
            <li>{t('import.ownerProof')}</li>
          </ul>
        </StepUpDialog>
      )}
    </div>
  );
}
