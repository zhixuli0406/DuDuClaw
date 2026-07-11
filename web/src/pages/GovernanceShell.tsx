import { useIntl } from 'react-intl';
import { Scale, Shield } from 'lucide-react';
import { TabbedMerge, type MergeTab } from '@/components/layout/TabbedMerge';
import { GovernancePage } from './GovernancePage';
import { WikiTrustPage } from './WikiTrustPage';

/**
 * GovernanceShell — `/manage/governance` merging governance workflow and wiki
 * trust into tabs (dashboard-redesign §3.2, WP6-T6.7). Enterprise + admin-gated.
 */
export function GovernanceShell() {
  const intl = useIntl();
  const tabs: MergeTab[] = [
    { id: 'governance', label: intl.formatMessage({ id: 'governance.tab.governance' }), icon: Scale, render: () => <GovernancePage /> },
    { id: 'wikiTrust', label: intl.formatMessage({ id: 'governance.tab.wikiTrust' }), icon: Shield, render: () => <WikiTrustPage /> },
  ];
  return <TabbedMerge tabs={tabs} />;
}
