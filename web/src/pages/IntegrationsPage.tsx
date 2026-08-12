import { useIntl } from 'react-intl';
import { useUrlState } from '@/lib/use-url-state';
import { Plug, Building2, UserSearch, Mail } from 'lucide-react';
import { Tabs, TabsList, TabsTab, TabsPanel, Separator } from '@/components/mds';
import { McpPage } from './McpPage';
import { McpKeysPage } from './McpKeysPage';
import { OdooPage } from './OdooPage';
import { IdentityPage } from './IdentityPage';
import { GoogleIntegrationPage } from './GoogleIntegrationPage';

/**
 * Google Workspace 分頁。v1.48.0 起開放：原本等的是原廠 OAuth App 驗證，
 * 但驗證只擋「使用者自建 OAuth client」那一條路，服務帳號網域委派與
 * Apps Script 橋接都不需要它（見 docs/guides/google-no-oauth-client.md），
 * 所以分頁本身沒有理由繼續隱藏。
 *
 * 後端工具面仍有各自的閘：`config.toml [integrations] google_workspace`
 * 控制 19 個工具對 agent 是否可見，設定頁在總開關沒開時會明講這件事。
 */
const GOOGLE_INTEGRATION_ENABLED = true;

const TAB_IDS = ['mcp', 'google', 'odoo', 'identity'] as const;
type TabId = (typeof TAB_IDS)[number];

/**
 * IntegrationsPage — the `/manage/integrations` surface. Four entries since
 * 2026-08-04 (D19): 工具伺服器 / Google / Odoo / 身分解析.
 *
 * What changed, and why: the page had grown to seven tabs, three of which
 * (Notion, GitHub, plus the MCP page's own OAuth sub-tab) were three different
 * doors onto the same authorization flow. Connecting a service is now something
 * you do on that service's own card inside 工具伺服器 — the card shows the
 * connection state and carries the 授權 button — instead of a separate page you
 * had to know existed. MCP access keys moved under the same tab rather than
 * getting a top-level slot of their own; nothing was removed.
 *
 * Older `?tab=notion|github|keys` links land on 工具伺服器, where all three now
 * live. The active tab still mirrors to `?tab=` so deep links keep working.
 */
export function IntegrationsPage() {
  const intl = useIntl();
  // P11 (state-as-URL): the active tab *is* `?tab=` — no shadow `useState`, so
  // Back/Forward and a pasted link can never disagree with what is rendered.
  // An unknown or hidden tab resolves to the first one rather than a blank panel.
  const hidden: TabId[] = GOOGLE_INTEGRATION_ENABLED ? [] : ['google'];
  const [tab, setTab] = useUrlState('tab', TAB_IDS[0], { allowed: TAB_IDS });
  const active: TabId = hidden.includes(tab) ? TAB_IDS[0] : tab;

  const onChange = (value: unknown) => setTab(value as TabId);

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
          <div className="space-y-8">
            <McpPage />
            <Separator />
            <McpKeysPage />
          </div>
        </TabsPanel>
        {GOOGLE_INTEGRATION_ENABLED && (
          <TabsPanel value="google">
            <GoogleIntegrationPage />
          </TabsPanel>
        )}
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
