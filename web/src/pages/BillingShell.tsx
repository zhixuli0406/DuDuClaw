import { useIntl } from 'react-intl';
import { CreditCard, Wallet } from 'lucide-react';
import { TabbedMerge, type MergeTab } from '@/components/layout/TabbedMerge';
import { BillingPage } from './BillingPage';
import { AccountsPage } from './AccountsPage';

/**
 * BillingShell — the `/manage/billing` surface merging billing/usage and
 * account rotation into tabs (dashboard-redesign §3.2, WP6-T6.3). Billing is
 * manager-visible; the account-rotation tab is admin-gated by its own RPCs.
 */
export function BillingShell() {
  const intl = useIntl();
  const tabs: MergeTab[] = [
    { id: 'billing', label: intl.formatMessage({ id: 'billing.tab.billing' }), icon: CreditCard, render: () => <BillingPage /> },
    { id: 'accounts', label: intl.formatMessage({ id: 'billing.tab.accounts' }), icon: Wallet, render: () => <AccountsPage /> },
  ];
  return <TabbedMerge tabs={tabs} />;
}
