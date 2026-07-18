import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { OrgChart } from '@/components/OrgChart';
import { OrgNodePanel } from '@/components/agent';
import type { AgentDetail } from '@/lib/api';
import { Users } from 'lucide-react';
import { CollectionPageHeader, CollectionPageState, Card } from '@/components/mds';
import { usePanel } from '@/components/ui';

export function OrgChartPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { agents, fetchAgents, pauseAgent, resumeAgent, loading, loaded } = useAgentsStore();
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
          onPause={async () => {
            await pauseAgent(selectedAgent.name);
            setSelectedAgent((prev) => (prev ? { ...prev, status: 'paused' } : null));
          }}
          onResume={async () => {
            await resumeAgent(selectedAgent.name);
            setSelectedAgent((prev) => (prev ? { ...prev, status: 'active' } : null));
          }}
        />
      ),
    });
  }, [selectedAgent, setPanel, clearPanel, navigate, pauseAgent, resumeAgent]);

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
