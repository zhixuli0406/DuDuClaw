import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { BookOpenIcon } from 'lucide-react';
import {
  CollectionPageHeader,
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
} from '@/components/mds';
import { KnowledgeHubPage } from './KnowledgeHubPage';
import { SharedWikiPage } from './SharedWikiPage';

type TabId = 'personal' | 'shared';

/**
 * KnowledgeShell — merges the personal wiki (KnowledgeHubPage) and the shared
 * wiki (SharedWikiPage) into one `/knowledge` surface with 個人 / 共享 line tabs
 * (spec §5.2). The shell owns the page header; each child renders header-less
 * via its `embedded` prop. The active tab mirrors to `?tab=` for deep links.
 */
export function KnowledgeShell() {
  const intl = useIntl();
  const [params, setParams] = useSearchParams();
  const active: TabId = params.get('tab') === 'shared' ? 'shared' : 'personal';

  const onChange = (id: TabId) => {
    const next = new URLSearchParams(params);
    next.set('tab', id);
    setParams(next, { replace: true });
  };

  return (
    <Tabs
      variant="line"
      value={active}
      onValueChange={(v) => onChange(v as TabId)}
      className="-mx-4 -mt-4 flex flex-1 flex-col gap-0 md:-mx-6 md:-mt-6"
    >
      <CollectionPageHeader
        hideTrigger
        icon={BookOpenIcon}
        title={intl.formatMessage({ id: 'nav.wiki' })}
      />
      <div className="flex h-12 shrink-0 items-center border-b border-surface-border px-4">
        <TabsList>
          <TabsTab value="personal">{intl.formatMessage({ id: 'knowledge.tab.personal' })}</TabsTab>
          <TabsTab value="shared">{intl.formatMessage({ id: 'knowledge.tab.shared' })}</TabsTab>
        </TabsList>
      </div>
      <TabsPanel value="personal" className="flex-1 p-4 md:p-6">
        <KnowledgeHubPage embedded />
      </TabsPanel>
      <TabsPanel value="shared" className="flex-1 p-4 md:p-6">
        <SharedWikiPage embedded />
      </TabsPanel>
    </Tabs>
  );
}
