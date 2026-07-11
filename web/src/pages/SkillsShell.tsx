import { useIntl } from 'react-intl';
import { Puzzle, Store } from 'lucide-react';
import { TabbedMerge, type MergeTab } from '@/components/layout/TabbedMerge';
import { SkillMarketPage } from './SkillMarketPage';
import { MarketplacePage } from './MarketplacePage';

/**
 * SkillsShell — merges the two former skill surfaces (SkillMarketPage +
 * MarketplacePage) into one `/skills` page with tabs (dashboard-redesign §5,
 * WP5-T5.2). Behaviour of each page is preserved; only the entry point converges.
 */
export function SkillsShell() {
  const intl = useIntl();
  const tabs: MergeTab[] = [
    { id: 'skills', label: intl.formatMessage({ id: 'skills.tab.mine' }), icon: Puzzle, render: () => <SkillMarketPage /> },
    { id: 'market', label: intl.formatMessage({ id: 'skills.tab.market' }), icon: Store, render: () => <MarketplacePage /> },
  ];
  return <TabbedMerge tabs={tabs} />;
}
