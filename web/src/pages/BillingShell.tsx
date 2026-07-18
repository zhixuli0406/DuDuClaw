import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { CreditCard, Wallet } from 'lucide-react';
import { Tabs, TabsList, TabsTab, TabsPanel } from '@/components/mds';
import { BillingPage } from './BillingPage';
import { AccountsPage } from './AccountsPage';

const TAB_IDS = ['billing', 'accounts'] as const;
type TabId = (typeof TAB_IDS)[number];

/**
 * BillingShell — the `/manage/billing` surface merging billing/usage and
 * account rotation into tabs (dashboard-redesign §3.2, WP6-T6.3). Billing is
 * manager-visible; the account-rotation tab is admin-gated by its own RPCs.
 * Mirrors the active tab to `?tab=` so deep links keep working (MDS Tabs
 * pattern shared with GovernanceShell, replacing the legacy TabbedMerge).
 */
export function BillingShell() {
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
          <TabsTab value="billing">
            <CreditCard />
            {intl.formatMessage({ id: 'billing.tab.billing' })}
          </TabsTab>
          <TabsTab value="accounts">
            <Wallet />
            {intl.formatMessage({ id: 'billing.tab.accounts' })}
          </TabsTab>
        </TabsList>
        <TabsPanel value="billing" keepMounted>
          <BillingPage />
        </TabsPanel>
        <TabsPanel value="accounts" keepMounted>
          <AccountsPage />
        </TabsPanel>
      </Tabs>
    </div>
  );
}
