import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { OrgChart } from '@/components/OrgChart';
import { OrgNodePanel } from '@/components/agent';
import type { AgentDetail } from '@/lib/api';
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
  const intl = useIntl();
  const navigate = useNavigate();
  const errorText = useErrorMessage();
  // `error` was never destructured here (P05 Blocker, phase-4 audit): a failed
  // roster fetch fell through to the "還沒有 AI 員工？" empty state, telling the
  // user to hire someone when the real problem was the connection.
  const { agents, fetchAgents, pauseAgent, resumeAgent, loading, loaded, error, clearError } =
    useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState<AgentDetail | null>(null);
  const { setPanel, clearPanel, setSheetOpen } = usePanel();

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  // Clear the right panel when leaving the page.
  useEffect(() => () => clearPanel(), [clearPanel]);

  const handleNodeClick = (agentName: string) => {
    const agent = agents.find((a) => a.name === agentName) ?? null;
    setSelectedAgent(agent);
    if (agent) setSheetOpen(true); // mobile: open the bottom sheet
  };

  // Push the selected staff card into the shared right PropertiesPanel (§5.4 T6.3).
  useEffect(() => {
    if (!selectedAgent) return;
    setPanel({
      title: selectedAgent.display_name,
      content: (
        <OrgNodePanel
          agent={selectedAgent}
          onOpenDetail={() => {
            clearPanel();
            navigate(`/agents/${encodeURIComponent(selectedAgent.name)}`);
          }}
          onEdit={() => {
            clearPanel();
            navigate(`/agents/${encodeURIComponent(selectedAgent.name)}/edit`);
          }}
          onPause={async () => {
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
  }, [selectedAgent, setPanel, clearPanel, navigate, pauseAgent, resumeAgent, intl, errorText]);

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        icon={Users}
        title={intl.formatMessage({ id: 'nav.org' })}
        count={agents.length}
        description={intl.formatMessage({ id: 'nav.org.desc' })}
      />

      {/* Team page = the org chart itself. The immersive world lives at its own
          /world entry (Sidebar), so this page no longer duplicates it as a tab. */}
      <div className="mx-auto w-full max-w-6xl p-6">
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
        ) : agents.length === 0 ? (
          <CollectionPageState state="empty" icon={Users} title={intl.formatMessage({ id: 'agents.empty' })} />
        ) : (
          <Card className="gap-0 p-2">
            <OrgChart
              agents={agents}
              onNodeClick={handleNodeClick}
              labels={{
                main: intl.formatMessage({ id: 'orgchart.legend.main' }),
                specialist: intl.formatMessage({ id: 'orgchart.legend.specialist' }),
                worker: intl.formatMessage({ id: 'orgchart.legend.worker' }),
                zoom: intl.formatMessage({ id: 'orgchart.zoom' }),
              }}
            />
          </Card>
        )}
      </div>
    </div>
  );
}
