import { useIntl } from 'react-intl';
import { Plug, KeyRound, Building2 } from 'lucide-react';
import { TabbedMerge, type MergeTab } from '@/components/layout/TabbedMerge';
import { McpPage } from './McpPage';
import { McpKeysPage } from './McpKeysPage';
import { OdooPage } from './OdooPage';

/**
 * IntegrationsPage — the `/manage/integrations` surface merging MCP servers,
 * MCP keys and Odoo into one tabbed page (dashboard-redesign §3.2, WP6-T6.2).
 * adm-gated (via ManageShell + RPC); function-first naming ("整合／工具連線")
 * per the §3 ruling on management-surface technical terms.
 */
export function IntegrationsPage() {
  const intl = useIntl();
  const tabs: MergeTab[] = [
    { id: 'mcp', label: intl.formatMessage({ id: 'integrations.tab.mcp' }), icon: Plug, render: () => <McpPage /> },
    { id: 'keys', label: intl.formatMessage({ id: 'integrations.tab.keys' }), icon: KeyRound, render: () => <McpKeysPage /> },
    { id: 'odoo', label: intl.formatMessage({ id: 'integrations.tab.odoo' }), icon: Building2, render: () => <OdooPage /> },
  ];
  return <TabbedMerge tabs={tabs} />;
}
