import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { Scale, Shield } from 'lucide-react';
import { Tabs, TabsList, TabsTab, TabsPanel } from '@/components/mds';
import { GovernancePage } from './GovernancePage';
import { WikiTrustPage } from './WikiTrustPage';

const TAB_IDS = ['governance', 'wikiTrust'] as const;
type TabId = (typeof TAB_IDS)[number];

/**
 * GovernanceShell — `/manage/governance` merging governance workflow and wiki
 * trust into tabs (dashboard-redesign §3.2, WP6-T6.7). Enterprise + admin-gated.
 * Mirrors the active tab to `?tab=` so deep links keep working (replicates the
 * legacy TabbedMerge behavior without pulling in the Calm Glass Tabs it used).
 */
export function GovernanceShell() {
  const intl = useIntl();
  const [params, setParams] = useSearchParams();
  const fromUrl = params.get('tab');
  const initial: TabId = TAB_IDS.includes(fromUrl as TabId) ? (fromUrl as TabId) : TAB_IDS[0];
  const [active, setActive] = useState<TabId>(initial);

  const onChange = (value: unknown) => {
    const id = value as TabId;
    setActive(id);
    const next = new URLSearchParams(params);
    next.set('tab', id);
    setParams(next, { replace: true });
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Tabs value={active} onValueChange={onChange} variant="line">
        <TabsList className="mb-4">
          <TabsTab value="governance">
            <Scale />
            {intl.formatMessage({ id: 'governance.tab.governance' })}
          </TabsTab>
          <TabsTab value="wikiTrust">
            <Shield />
            {intl.formatMessage({ id: 'governance.tab.wikiTrust' })}
          </TabsTab>
        </TabsList>
        <TabsPanel value="governance" keepMounted>
          <GovernancePage />
        </TabsPanel>
        <TabsPanel value="wikiTrust" keepMounted>
          <WikiTrustPage />
        </TabsPanel>
      </Tabs>
    </div>
  );
}
