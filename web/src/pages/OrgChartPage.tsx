import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { OrgChart } from '@/components/OrgChart';
import { OrgNodePanel } from '@/components/agent';
import type { AgentDetail } from '@/lib/api';
import { Network, GitBranch, Globe2 } from 'lucide-react';
import { Page, PageHeader, Card, Tabs, usePanel } from '@/components/ui';

export function OrgChartPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { agents, fetchAgents, pauseAgent, resumeAgent } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState<AgentDetail | null>(null);
  // 組織圖 | 世界 view toggle (dashboard-redesign §5.4 / WP10). Both read the same
  // agents data — switching never refetches.
  const [view, setView] = useState<'chart' | 'world'>('chart');
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
    <Page>
      <PageHeader
        icon={Network}
        title={intl.formatMessage({ id: 'nav.org' })}
        subtitle={intl.formatMessage(
          { id: 'orgchart.subtitle' },
          { count: agents.length },
        )}
      />

      <Tabs
        className="mb-4"
        value={view}
        onChange={(id) => setView(id as 'chart' | 'world')}
        items={[
          { id: 'chart', label: intl.formatMessage({ id: 'orgchart.view.chart' }), icon: GitBranch },
          { id: 'world', label: intl.formatMessage({ id: 'orgchart.view.world' }), icon: Globe2 },
        ]}
      />

      {view === 'chart' ? (
        <Card padded={false} bodyClassName="p-2">
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
      ) : (
        // The heavy world scene lives on its own full-bleed /world page; the Org
        // "世界" tab is just a link there so the PixiJS engine mounts once, not in
        // three places (Home band + this tab + /world).
        <Card>
          <div className="flex flex-col items-center gap-3 py-10 text-center">
            <span className="grid h-14 w-14 place-items-center rounded-2xl bg-amber-500/10 text-amber-600 dark:text-amber-400">
              <Globe2 className="h-7 w-7" />
            </span>
            <p className="text-base font-semibold text-stone-700 dark:text-stone-100">
              {intl.formatMessage({ id: 'orgchart.world.title' })}
            </p>
            <p className="max-w-sm text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'orgchart.world.desc' })}
            </p>
            <button
              type="button"
              onClick={() => navigate('/world')}
              className="mt-1 inline-flex items-center gap-1.5 rounded-lg bg-amber-500 px-4 py-2 text-sm font-semibold text-white transition-transform hover:bg-amber-600 active:scale-[0.98]"
            >
              <Globe2 className="h-4 w-4" />
              {intl.formatMessage({ id: 'orgchart.world.open' })}
            </button>
          </div>
        </Card>
      )}
    </Page>
  );
}
