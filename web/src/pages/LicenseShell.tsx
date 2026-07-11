import { useIntl } from 'react-intl';
import { KeyRound, Handshake } from 'lucide-react';
import { TabbedMerge, type MergeTab } from '@/components/layout/TabbedMerge';
import { LicensePage } from './LicensePage';
import { PartnerPortalPage } from './PartnerPortalPage';

/**
 * LicenseShell — `/manage/license` merging license status and the partner
 * portal into tabs (dashboard-redesign §3.2, WP6-T6.9). License is
 * manager-visible; the partner tab is enterprise-gated by its own RPCs.
 */
export function LicenseShell() {
  const intl = useIntl();
  const tabs: MergeTab[] = [
    { id: 'license', label: intl.formatMessage({ id: 'license.tab.license' }), icon: KeyRound, render: () => <LicensePage /> },
    { id: 'partner', label: intl.formatMessage({ id: 'license.tab.partner' }), icon: Handshake, render: () => <PartnerPortalPage /> },
  ];
  return <TabbedMerge tabs={tabs} />;
}
