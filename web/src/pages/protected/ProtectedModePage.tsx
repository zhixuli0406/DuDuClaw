import { useCallback, useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router';
import { ShieldCheck } from 'lucide-react';
import { api } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import { useProtectedMode } from '@/hooks/useProtectedMode';
import { IsolationReadinessBanner, type IsolationNextSteps } from '@/components/IsolationReadinessBanner';
import { Empty, Tabs, TabsList, TabsPanel, TabsTab } from '@/components/mds';
import { StepUpProvider } from './step-up';
import { EmployeesTab } from './EmployeesTab';
import { ConnectorsTab } from './ConnectorsTab';
import { TenantSourceTab } from './TenantSourceTab';
import { AuditTab } from './AuditTab';
import {
  PROTECTED_MODE_PATH,
  PROTECTED_TABS,
  PageTrail,
  ProtectedPageProvider,
  useT,
  type Person,
  type ProtectedTab,
  type WizardPreset,
} from './shared';

/** Readiness reasons about connectors land on this page's own tab. */
const NEXT_STEPS: IsolationNextSteps = {
  connector_setup_required: { hint: 'isolationReadiness.hint.connector', to: `${PROTECTED_MODE_PATH}?tab=connectors` },
  revoked: { hint: 'isolationReadiness.hint.revoked', to: `${PROTECTED_MODE_PATH}?tab=connectors` },
};

function isTab(value: string | null): value is ProtectedTab {
  return value !== null && (PROTECTED_TABS as readonly string[]).includes(value);
}

/**
 * 設定 › 受保護模式 (v1.81 F5 part A): protected AI employees and staff
 * requests, connector accounts, the Google tenant source and their records.
 * Admin only (route guard + every RPC re-checks Admin and MFA). The tab is
 * kept in `?tab=` so readiness links and bookmarks land on the right one.
 */
export function ProtectedModePage() {
  const t = useT();
  const statusLoaded = useSystemStore((s) => s.status !== null);
  const personal = useSystemStore((s) => s.status?.edition_profile === 'personal');
  const protectedMode = useProtectedMode();
  const connection = useConnectionStore((s) => s.state);
  const [params, setParams] = useSearchParams();
  const requested = params.get('tab');
  const tab: ProtectedTab = isTab(requested) ? requested : 'employees';
  const [people, setPeople] = useState<Map<string, Person>>(new Map());
  const [wizardPreset, setWizardPreset] = useState<WizardPreset | null>(null);

  useEffect(() => {
    if (connection !== 'authenticated' || !protectedMode) return;
    let cancelled = false;
    api.users.list()
      .then((result) => {
        if (cancelled) return;
        setPeople(new Map((result?.users ?? []).map((user) => [user.id, { name: user.display_name, email: user.email }])));
      })
      // Names are presentation only; rows fall back to a neutral label.
      .catch(() => {});
    return () => { cancelled = true; };
  }, [connection, protectedMode]);

  const setTab = useCallback((next: ProtectedTab) => {
    setParams((current) => {
      const copy = new URLSearchParams(current);
      copy.set('tab', next);
      return copy;
    }, { replace: true });
  }, [setParams]);

  const goTo = useCallback((next: ProtectedTab, wizard?: WizardPreset) => {
    if (wizard) setWizardPreset(wizard);
    setTab(next);
  }, [setTab]);

  const context = useMemo(() => ({ people, goTo }), [people, goTo]);
  const clearPreset = useCallback(() => setWizardPreset(null), []);

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      <header className="flex min-w-0 items-start gap-2">
        <ShieldCheck className="mt-0.5 size-5 shrink-0 text-muted-foreground" aria-hidden="true" />
        <div className="min-w-0 space-y-1">
          <PageTrail trail={t('trail')} newIn="1.81.0" />
          <h1 className="text-base font-medium">{t('title')}</h1>
          <p className="text-sm text-muted-foreground">{t('description')}</p>
        </div>
      </header>

      {!statusLoaded ? (
        <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>
      ) : personal || !protectedMode ? (
        <Empty icon={ShieldCheck} title={t('inactive.title')} description={t(personal ? 'inactive.personal' : 'inactive.description')} />
      ) : (
        <ProtectedPageProvider value={context}>
          <StepUpProvider>
            <IsolationReadinessBanner nextSteps={NEXT_STEPS} />
            <Tabs value={tab} onValueChange={(value) => { if (isTab(value as string)) setTab(value as ProtectedTab); }} variant="line">
              <div className="overflow-x-auto pb-1.5">
                <TabsList aria-label={t('tabs.label')}>
                  {PROTECTED_TABS.map((name) => (
                    <TabsTab key={name} value={name} className="whitespace-nowrap">{t(`tabs.${name}`)}</TabsTab>
                  ))}
                </TabsList>
              </div>
              <TabsPanel value="employees" className="pt-4"><EmployeesTab /></TabsPanel>
              <TabsPanel value="connectors" className="pt-4">
                <ConnectorsTab wizardPreset={wizardPreset} onWizardPresetUsed={clearPreset} />
              </TabsPanel>
              <TabsPanel value="tenant" className="pt-4"><TenantSourceTab /></TabsPanel>
              <TabsPanel value="audit" className="pt-4"><AuditTab /></TabsPanel>
            </Tabs>
          </StepUpProvider>
        </ProtectedPageProvider>
      )}
    </div>
  );
}
