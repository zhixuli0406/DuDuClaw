import { useState, type ReactNode } from 'react';
import { useSearchParams } from 'react-router';
import { Tabs, type TabItem } from '@/components/ui';

export interface MergeTab extends TabItem {
  /** The page/body rendered when this tab is active. */
  render: () => ReactNode;
}

/**
 * TabbedMerge — the shell that collapses several previously-separate pages into
 * one multi-tab surface (dashboard-redesign §5.2/§6.2: Skills+Marketplace,
 * Knowledge+SharedWiki, Integrations MCP+Keys+Odoo, Billing+Accounts, …).
 *
 * IA rewrite, behaviour preserved: each tab renders the existing page component
 * untouched — no store/api change. The active tab is mirrored to `?tab=` so
 * deep links and the ⌘K palette can target a specific tab.
 */
export function TabbedMerge({ tabs, className }: { tabs: readonly MergeTab[]; className?: string }) {
  const [params, setParams] = useSearchParams();
  const fromUrl = params.get('tab');
  const initial = tabs.find((t) => t.id === fromUrl)?.id ?? tabs[0]?.id ?? '';
  const [active, setActive] = useState(initial);

  const onChange = (id: string) => {
    setActive(id);
    const next = new URLSearchParams(params);
    next.set('tab', id);
    setParams(next, { replace: true });
  };

  const current = tabs.find((t) => t.id === active) ?? tabs[0];

  return (
    <div className={className}>
      <Tabs items={tabs} value={current?.id ?? ''} onChange={onChange} className="mb-4" />
      {current?.render()}
    </div>
  );
}
