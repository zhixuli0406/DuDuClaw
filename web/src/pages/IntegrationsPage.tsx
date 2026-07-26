import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { Plug, KeyRound, Building2, UserSearch, Mail, FileText, Github } from 'lucide-react';
import { Tabs, TabsList, TabsTab, TabsPanel } from '@/components/mds';
import { McpPage } from './McpPage';
import { McpKeysPage } from './McpKeysPage';
import { OdooPage } from './OdooPage';
import { IdentityPage } from './IdentityPage';
import { GoogleIntegrationPage } from './GoogleIntegrationPage';
import { NotionIntegrationPage } from './NotionIntegrationPage';
import { GitHubIntegrationPage } from './GitHubIntegrationPage';

/** Google Workspace 分頁隨原廠 OAuth App 驗證進度開放 — 下個版本翻 true
 *  （後端工具面另有 config.toml [integrations] google_workspace 旗標）。 */
const GOOGLE_INTEGRATION_ENABLED = false;

const TAB_IDS = ['mcp', 'google', 'notion', 'github', 'keys', 'odoo', 'identity'] as const;
type TabId = (typeof TAB_IDS)[number];

/**
 * IntegrationsPage — the `/manage/integrations` surface merging MCP servers,
 * MCP keys and Odoo into one tabbed page (dashboard-redesign §3.2, WP6-T6.2).
 * adm-gated (via ManageShell + RPC); function-first naming ("整合／工具連線")
 * per the §3 ruling on management-surface technical terms. Mirrors the active
 * tab to `?tab=` (replicates the legacy TabbedMerge behavior on local mds Tabs,
 * matching GovernanceShell / BillingShell). Panels mount lazily — only the
 * active page is rendered, preserving TabbedMerge's single-mount behavior.
 */
export function IntegrationsPage() {
  const intl = useIntl();
  const [params, setParams] = useSearchParams();
  const fromUrl = params.get('tab');
  const hidden: TabId[] = GOOGLE_INTEGRATION_ENABLED ? [] : ['google'];
  const initial: TabId =
    TAB_IDS.includes(fromUrl as TabId) && !hidden.includes(fromUrl as TabId)
      ? (fromUrl as TabId)
      : TAB_IDS[0];
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
          <TabsTab value="mcp">
            <Plug />
            {intl.formatMessage({ id: 'integrations.tab.mcp' })}
          </TabsTab>
          {GOOGLE_INTEGRATION_ENABLED && (
            <TabsTab value="google">
              <Mail />
              {intl.formatMessage({ id: 'integrations.tab.google' })}
            </TabsTab>
          )}
          <TabsTab value="notion">
            <FileText />
            {intl.formatMessage({ id: 'integrations.tab.notion' })}
          </TabsTab>
          <TabsTab value="github">
            <Github />
            {intl.formatMessage({ id: 'integrations.tab.github' })}
          </TabsTab>
          <TabsTab value="keys">
            <KeyRound />
            {intl.formatMessage({ id: 'integrations.tab.keys' })}
          </TabsTab>
          <TabsTab value="odoo">
            <Building2 />
            {intl.formatMessage({ id: 'integrations.tab.odoo' })}
          </TabsTab>
          <TabsTab value="identity">
            <UserSearch />
            {intl.formatMessage({ id: 'integrations.tab.identity' })}
          </TabsTab>
        </TabsList>
        <TabsPanel value="mcp">
          <McpPage />
        </TabsPanel>
        {GOOGLE_INTEGRATION_ENABLED && (
          <TabsPanel value="google">
            <GoogleIntegrationPage />
          </TabsPanel>
        )}
        <TabsPanel value="notion">
          <NotionIntegrationPage />
        </TabsPanel>
        <TabsPanel value="github">
          <GitHubIntegrationPage />
        </TabsPanel>
        <TabsPanel value="keys">
          <McpKeysPage />
        </TabsPanel>
        <TabsPanel value="odoo">
          <OdooPage />
        </TabsPanel>
        <TabsPanel value="identity">
          <IdentityPage />
        </TabsPanel>
      </Tabs>
    </div>
  );
}
