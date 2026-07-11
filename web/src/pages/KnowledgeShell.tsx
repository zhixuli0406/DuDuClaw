import { useIntl } from 'react-intl';
import { BookOpen, Globe } from 'lucide-react';
import { TabbedMerge, type MergeTab } from '@/components/layout/TabbedMerge';
import { KnowledgeHubPage } from './KnowledgeHubPage';
import { SharedWikiPage } from './SharedWikiPage';

/**
 * KnowledgeShell — merges personal wiki (KnowledgeHubPage) and shared wiki
 * (SharedWikiPage) into one `/knowledge` page with 個人 / 共享 tabs
 * (dashboard-redesign §3.2, WP5-T5.3).
 */
export function KnowledgeShell() {
  const intl = useIntl();
  const tabs: MergeTab[] = [
    { id: 'personal', label: intl.formatMessage({ id: 'knowledge.tab.personal' }), icon: BookOpen, render: () => <KnowledgeHubPage /> },
    { id: 'shared', label: intl.formatMessage({ id: 'knowledge.tab.shared' }), icon: Globe, render: () => <SharedWikiPage /> },
  ];
  return <TabbedMerge tabs={tabs} />;
}
