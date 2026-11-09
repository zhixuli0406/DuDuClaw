import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { AccountTab } from '@/components/settings/sections/AccountTab';
import { Tabs, TabsList, TabsPanel, TabsTab } from '@/components/mds';
import { useSystemStore } from '@/stores/system-store';
import { isNewFeature } from '@/apps/registry';
import { MY_PERSONAL_TAB, MyPersonalEmployeesTab } from './account/MyPersonalEmployeesTab';

export function AccountPage() {
  const intl = useIntl();
  // v1.81 F5/F6e-UI: protected mode (native Linux isolation, TEAM) adds the
  // 我的個人 AI 員工 tab. Everywhere else the page is unchanged.
  const status = useSystemStore((s) => s.status);
  const protectedMode = status?.isolation_readiness != null;
  const [params, setParams] = useSearchParams();
  const tab = protectedMode && params.get('tab') === MY_PERSONAL_TAB.id ? MY_PERSONAL_TAB.id : 'account';
  const header = (
    <div>
      <h1 className="text-2xl font-semibold">{intl.formatMessage({ id: 'nav.account' })}</h1>
      <p className="mt-2 text-sm text-muted-foreground">{intl.formatMessage({ id: 'nav.account.desc' })}</p>
    </div>
  );
  if (!protectedMode) {
    return (
      <div className="mx-auto max-w-4xl space-y-6 p-6">
        {header}
        <AccountTab />
      </div>
    );
  }
  const showNew = isNewFeature(MY_PERSONAL_TAB.newIn, status?.version ?? null);
  return (
    <div className="mx-auto max-w-4xl space-y-6 p-4 sm:p-6">
      {header}
      <Tabs
        variant="line"
        value={tab}
        onValueChange={(value) => {
          const next = new URLSearchParams(params);
          if (value === MY_PERSONAL_TAB.id) next.set('tab', MY_PERSONAL_TAB.id);
          else next.delete('tab');
          setParams(next, { replace: true });
        }}
      >
        <TabsList className="max-w-full overflow-x-auto border-b border-surface-border pb-1">
          <TabsTab value="account">{intl.formatMessage({ id: 'myPersonal.tab.account' })}</TabsTab>
          <TabsTab value={MY_PERSONAL_TAB.id}>
            {intl.formatMessage({ id: MY_PERSONAL_TAB.labelId })}
            {showNew && (
              <span className="shrink-0 rounded-full bg-brand/15 px-1.5 py-px text-[10px] font-medium text-brand">
                {intl.formatMessage({ id: 'nav.badge.new' })}
              </span>
            )}
          </TabsTab>
        </TabsList>
        <TabsPanel value="account" className="pt-4">
          <AccountTab />
        </TabsPanel>
        <TabsPanel value={MY_PERSONAL_TAB.id} className="pt-4">
          <MyPersonalEmployeesTab />
        </TabsPanel>
      </Tabs>
    </div>
  );
}
