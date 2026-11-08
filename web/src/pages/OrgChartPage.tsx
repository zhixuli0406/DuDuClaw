import { canSelectAgent, useAgentGrantPresent } from '@/stores/callable-agents-store';
import { useVisibleAgents, useAgentVisibilityUnrestricted } from '@/lib/data-scope';
import { Link } from 'react-router';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { OrgChart } from '@/components/OrgChart';
import { OrgNodePanel } from '@/components/agent';
import { OrgPlacementPanel, type PlacementDraft } from '@/components/org/OrgPlacementPanel';
import { OrgPendingTable } from '@/components/org/OrgPendingTable';
import { hasMinRole } from '@/lib/roles';
import { normPlacement, pendingStatus, type OrgChangeRow } from '@/lib/org-change';
import { api, type AgentDetail, type OrgChartAgent, type OrgPendingChange } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { Users } from 'lucide-react';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  ErrorState,
  useErrorMessage,
} from '@/components/mds';
import { toast } from '@/lib/toast';
import { usePanel } from '@/components/ui';

export function OrgChartPage() {
  const user = useAuthStore(s => s.user);
  return user?.role === 'employee'
    ? <EmployeeOrgChart key={user.id} />
    : <ManagedOrgChart />;
}

function EmployeeOrgChart() {
  const intl = useIntl();
  const connection = useConnectionStore(s => s.state);
  const [directoryAgents, setAgents] = useState<OrgChartAgent[]>([]);
  const visible = useVisibleAgents();
  const unrestricted = useAgentVisibilityUnrestricted();
  const agents = directoryAgents.filter(a => unrestricted || visible.some(v => v.name === a.name));
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let active = true;
    setAgents([]);
    setLoading(true);
    setError(null);
    if (connection === 'authenticated') {
      void api.agents.orgChart().then(result => {
        if (active) setAgents(result.agents);
      }).catch(err => {
        if (active) setError(err);
      }).finally(() => {
        if (active) setLoading(false);
      });
    }
    return () => { active = false; };
  }, [connection, attempt]);
  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader icon={Users} title={intl.formatMessage({ id: 'nav.org' })}
        count={agents.length} description={intl.formatMessage({ id: 'nav.org.desc' })} />
      <div className="mx-auto w-full max-w-6xl p-6">
        {loading ? <CollectionPageState state="loading" />
          : error != null ? <ErrorState icon={Users} error={error} onRetry={() => setAttempt(n => n + 1)} />
          : agents.length === 0 ? <CollectionPageState state="empty" icon={Users} title={intl.formatMessage({ id: 'agents.empty' })} />
          : <Card className="gap-0 p-2"><OrgChart agents={agents} labels={{
            main: intl.formatMessage({ id: 'orgchart.legend.main' }),
            specialist: intl.formatMessage({ id: 'orgchart.legend.specialist' }),
            worker: intl.formatMessage({ id: 'orgchart.legend.worker' }),
            zoom: intl.formatMessage({ id: 'orgchart.zoom' }),
          }} /></Card>}
      </div>
    </div>
  );
}

function ManagedOrgChart() {
  const intl = useIntl();
  const navigate = useNavigate();
  const errorText = useErrorMessage();
  // `error` was never destructured here (P05 Blocker, phase-4 audit): a failed
  // roster fetch fell through to the "還沒有 AI 員工？" empty state, telling the
  // user to hire someone when the real problem was the connection.
  const { fetchAgents, pauseAgent, resumeAgent, loading, loaded, error, clearError } =
    useAgentsStore();
  const agents = useVisibleAgents(false);
  const visibleAgents = useVisibleAgents();
  const teamAgents = visibleAgents.filter(a => a.readOnly);
  const [selectedAgent, setSelectedAgent] = useState<AgentDetail | null>(null);
  const { setPanel, clearPanel, setSheetOpen } = usePanel();

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  // ── RM-027: org editing ──────────────────────────────────────
  const user = useAuthStore((s) => s.user);
  const isAdmin = hasMinRole(user?.role, 'admin');
  const canPropose = hasMinRole(user?.role, 'manager');
  const [waiting, setWaiting] = useState<OrgPendingChange[]>([]);
  const [settled, setSettled] = useState<OrgChangeRow[]>([]);
  const [draft, setDraft] = useState<PlacementDraft | null>(null);
  const draftNonce = useRef(0);
  const seenWaiting = useRef<OrgPendingChange[]>([]);

  const loadPending = useCallback(async () => {
    if (!canPropose) return;
    try {
      const r = await api.org.pendingChanges();
      const next = Array.isArray(r?.pending) ? r.pending : [];
      // A request that left the waiting list was decided: it counts as 已同意
      // when the employee now sits where it asked, otherwise 已拒絕.
      const gone = seenWaiting.current.filter((p) => !next.some((n) => n.approval_id === p.approval_id));
      if (gone.length > 0) {
        const roster = useAgentsStore.getState().agents;
        setSettled((prev) => [
          ...gone.map((change): OrgChangeRow => {
            const now = roster.find((a) => a.name === change.agent_id);
            const after = normPlacement(change.after);
            const applied = !!now && (now.reports_to ?? '') === after.reports_to && (now.department ?? '') === after.department;
            return { change, status: applied ? 'approved' : 'rejected' };
          }),
          ...prev,
        ].slice(0, 5));
      }
      seenWaiting.current = next;
      setWaiting(next);
    } catch {
      // Waiting requests are an extra; the chart itself still works without them.
    }
  }, [canPropose]);

  useEffect(() => {
    void loadPending();
  }, [loadPending]);

  const refreshAll = useCallback(async () => {
    await fetchAgents();
    await loadPending();
  }, [fetchAgents, loadPending]);

  const pendingByAgent = useMemo(() => {
    const m = new Map<string, OrgPendingChange>();
    for (const p of waiting) if (!p.stale && !m.has(p.agent_id)) m.set(p.agent_id, p);
    for (const p of waiting) if (!m.has(p.agent_id)) m.set(p.agent_id, p);
    return m;
  }, [waiting]);
  const pendingNames = useMemo(() => new Set(waiting.map((p) => p.agent_id)), [waiting]);
  const tableRows = useMemo<OrgChangeRow[]>(() => {
    const mine = (c: OrgPendingChange) => isAdmin || c.proposer === user?.id;
    return [
      ...waiting.filter(mine).map((change): OrgChangeRow => ({ change, status: pendingStatus(change) })),
      ...settled.filter((r) => mine(r.change)),
    ];
  }, [waiting, settled, isAdmin, user?.id]);
  const labelOf = useCallback(
    (id: string) => agents.find((a) => a.name === id)?.display_name ?? id,
    [agents],
  );

  const selectedAllowed = useAgentGrantPresent(selectedAgent?.name);
  useEffect(() => {
    if (selectedAgent && !selectedAllowed) {
      setSelectedAgent(null);
      clearPanel();
      setSheetOpen(false);
    }
  }, [selectedAgent, selectedAllowed, clearPanel, setSheetOpen]);

  // Clear the right panel when leaving the page.
  useEffect(() => () => clearPanel(), [clearPanel]);

  const handleNodeClick = (agentName: string) => {
    if (!canSelectAgent(agentName)) return;
    const agent = agents.find((a) => a.name === agentName) ?? null;
    setSelectedAgent(agent);
    setDraft(null);
    if (agent) setSheetOpen(true); // mobile: open the bottom sheet
  };

  // Dropping a node onto a manager opens that employee's 「調整位置」 with the
  // manager pre-selected; nothing is sent until the user confirms there.
  const handleReparent = useCallback((agentName: string, managerName: string) => {
    if (!canSelectAgent(agentName)) return;
    const agent = agents.find((a) => a.name === agentName) ?? null;
    if (!agent) return;
    setSelectedAgent(agent);
    setSheetOpen(true);
    draftNonce.current += 1;
    setDraft({ agent: agentName, manager: managerName, nonce: draftNonce.current });
  }, [agents, setSheetOpen]);

  // Keep the open card in step with the roster after a change is applied.
  useEffect(() => {
    if (!selectedAgent) return;
    const fresh = agents.find((a) => a.name === selectedAgent.name);
    if (fresh && ((fresh.reports_to ?? '') !== (selectedAgent.reports_to ?? '') || (fresh.department ?? '') !== (selectedAgent.department ?? ''))) {
      setSelectedAgent(fresh);
    }
  }, [agents, selectedAgent]);

  // Push the selected staff card into the shared right PropertiesPanel (§5.4 T6.3).
  useEffect(() => {
    if (!selectedAgent || !selectedAllowed) return;
    setPanel({
      title: selectedAgent.display_name,
      content: (
        <OrgNodePanel
          agent={selectedAgent}
          placement={canPropose ? (
            <OrgPlacementPanel
              key={selectedAgent.name}
              agent={selectedAgent}
              agents={agents}
              isAdmin={isAdmin}
              pending={pendingByAgent.get(selectedAgent.name)}
              draft={draft}
              onChanged={() => void refreshAll()}
              onRefresh={() => { clearError(); void fetchAgents(); }}
            />
          ) : undefined}
          onOpenDetail={() => {
            clearPanel();
            navigate(`/agents/${encodeURIComponent(selectedAgent.name)}`);
          }}
          onEdit={() => {
            clearPanel();
            navigate(`/agents/${encodeURIComponent(selectedAgent.name)}/edit`);
          }}
          onPause={async () => {
            if (!canSelectAgent(selectedAgent.name)) return;
            try {
              await pauseAgent(selectedAgent.name);
            } catch (e) {
              toast.error(
                intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: errorText(e) }),
              );
              return;
            }
            setSelectedAgent((prev) => (prev ? { ...prev, status: 'paused' } : null));
          }}
          onResume={async () => {
            if (!canSelectAgent(selectedAgent.name)) return;
            try {
              await resumeAgent(selectedAgent.name);
            } catch (e) {
              toast.error(
                intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: errorText(e) }),
              );
              return;
            }
            setSelectedAgent((prev) => (prev ? { ...prev, status: 'active' } : null));
          }}
        />
      ),
    });
  }, [selectedAgent, selectedAllowed, setPanel, clearPanel, navigate, pauseAgent, resumeAgent, intl, errorText,
    canPropose, agents, isAdmin, pendingByAgent, draft, refreshAll, clearError, fetchAgents]);

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        icon={Users}
        title={intl.formatMessage({ id: 'nav.org' })}
        count={visibleAgents.length}
        description={intl.formatMessage({ id: 'nav.org.desc' })}
      />

      {/* Team page = the org chart itself. The immersive world lives at its own
          /world entry (Sidebar), so this page no longer duplicates it as a tab. */}
      <div className="mx-auto w-full max-w-6xl p-6">
        {teamAgents.length > 0 && <ul className="mb-4 space-y-2">{teamAgents.map(a => <li key={a.name} className="text-sm"><Link className="text-brand hover:underline" to={`/runs?agent=${encodeURIComponent(a.name)}`}>{a.display_name}</Link><span className="ml-2 text-muted-foreground">{intl.formatMessage({ id: 'agents.teamMember' }, { name: a.teamMember })}</span></li>)}</ul>}
        {!loaded && loading ? (
          <CollectionPageState state="loading" />
        ) : error != null && agents.length === 0 ? (
          <ErrorState
            icon={Users}
            error={error}
            onRetry={() => {
              clearError();
              void fetchAgents();
            }}
          />
        ) : visibleAgents.length === 0 ? (
          <CollectionPageState state="empty" icon={Users} title={intl.formatMessage({ id: 'agents.empty' })} />
        ) : agents.length > 0 ? (
          <>
            <Card className="gap-0 p-2">
              <OrgChart
                agents={agents}
                onNodeClick={handleNodeClick}
                pendingNames={canPropose ? pendingNames : undefined}
                selectedName={selectedAgent?.name}
                onReparent={canPropose ? handleReparent : undefined}
                labels={{
                  main: intl.formatMessage({ id: 'orgchart.legend.main' }),
                  specialist: intl.formatMessage({ id: 'orgchart.legend.specialist' }),
                  worker: intl.formatMessage({ id: 'orgchart.legend.worker' }),
                  zoom: intl.formatMessage({ id: 'orgchart.zoom' }),
                  pending: intl.formatMessage({ id: 'orgEdit.status.pending' }),
                  dropHint: (manager: string) => intl.formatMessage({ id: 'orgEdit.dropHint' }, { manager }),
                }}
              />
            </Card>
            {canPropose && (
              <p className="mt-2 text-xs text-muted-foreground">{intl.formatMessage({ id: 'orgEdit.hint' })}</p>
            )}
            {canPropose && tableRows.length > 0 && (
              <div className="mt-5">
                <OrgPendingTable rows={tableRows} labelOf={labelOf} currentUserId={user?.id} />
              </div>
            )}
          </>
        ) : null}
      </div>
    </div>
  );
}
