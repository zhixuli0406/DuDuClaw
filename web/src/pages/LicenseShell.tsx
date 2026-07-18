import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { KeyRound, Handshake } from 'lucide-react';
import { Tabs, TabsList, TabsTab, TabsPanel } from '@/components/mds';
import { LicensePage } from './LicensePage';
import { PartnerPortalPage } from './PartnerPortalPage';

const TAB_IDS = ['license', 'partner'] as const;
type TabId = (typeof TAB_IDS)[number];

/**
 * LicenseShell — `/manage/license` merging license status and the partner
 * portal into tabs (dashboard-redesign §3.2, WP6-T6.9). License is
 * manager-visible; the partner tab is enterprise-gated by its own RPCs.
 * Mirrors the active tab to `?tab=` (MDS Tabs pattern shared with
 * GovernanceShell, replacing the legacy TabbedMerge).
 */
export function LicenseShell() {
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
          <TabsTab value="license">
            <KeyRound />
            {intl.formatMessage({ id: 'license.tab.license' })}
          </TabsTab>
          <TabsTab value="partner">
            <Handshake />
            {intl.formatMessage({ id: 'license.tab.partner' })}
          </TabsTab>
        </TabsList>
        <TabsPanel value="license" keepMounted>
          <LicensePage />
        </TabsPanel>
        <TabsPanel value="partner" keepMounted>
          <PartnerPortalPage />
        </TabsPanel>
      </Tabs>
    </div>
  );
}
