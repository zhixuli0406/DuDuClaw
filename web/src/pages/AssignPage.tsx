import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useLocation, useNavigate, useSearchParams } from 'react-router';

import { Tabs, TabsList, TabsPanel, TabsTab } from '@/components/mds';
import { ComplexAssignTab } from '@/components/assign/ComplexAssignTab';
import { SkillAssignTab } from '@/components/assign/SkillAssignTab';
import { initialAssignTab, readAssignPrefill, type AssignTab } from '@/lib/assign-entry';

/**
 * `/assign` — the one 交辦任務 surface (replaces the former AssignSheet modal).
 *
 * Two tabs: 「一般」 hands a skill to the employee who last ran it
 * (`tasks.skill_assign`); 「複雜任務」 is the full form (`tasks.goal_create`,
 * 問一問 / 交辦 / 想一想). A seeded entry point (router state, see
 * `lib/assign-entry`) opens 複雜任務 with the seed; `?tab=complex` selects it
 * too. Both editions.
 */
export function AssignPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const location = useLocation();
  const [searchParams] = useSearchParams();
  const prefill = readAssignPrefill(location.state);

  // Re-derived per navigation: a new entry (another seed) lands on its tab.
  const [tabFor, setTabFor] = useState<{ key: string; tab: AssignTab }>(() => ({
    key: location.key,
    tab: initialAssignTab(prefill, searchParams.get('tab')),
  }));
  const tab = tabFor.key === location.key ? tabFor.tab : initialAssignTab(prefill, searchParams.get('tab'));
  const setTab = (next: AssignTab) => setTabFor({ key: location.key, tab: next });

  // Back to wherever the person came from; straight to the home page when
  // /assign was opened directly.
  const cancel = () => {
    if (location.key !== 'default') navigate(-1);
    else navigate('/');
  };

  return (
    <div className="mx-auto w-full max-w-[760px]">
      <header className="mb-5">
        <h1 className="text-xl font-semibold text-foreground">{intl.formatMessage({ id: 'assign.title' })}</h1>
        <p className="mt-1 text-sm text-muted-foreground">{intl.formatMessage({ id: 'assign.subtitle' })}</p>
      </header>

      <Tabs variant="line" value={tab} onValueChange={(v) => setTab(v as AssignTab)}>
        <TabsList
          className="w-full justify-start gap-4 border-b border-surface-border pb-1"
          aria-label={intl.formatMessage({ id: 'assign.tabs' })}
        >
          <TabsTab value="general" className="min-h-[44px] px-1 sm:min-h-9">
            {intl.formatMessage({ id: 'assign.tab.general' })}
          </TabsTab>
          <TabsTab value="complex" className="min-h-[44px] px-1 sm:min-h-9">
            {intl.formatMessage({ id: 'assign.tab.complex' })}
          </TabsTab>
        </TabsList>
        {/* Both panels stay mounted so switching tabs keeps what was typed. */}
        <TabsPanel value="general" keepMounted className="pt-4">
          <SkillAssignTab key={`g-${location.key}`} onCancel={cancel} />
        </TabsPanel>
        <TabsPanel value="complex" keepMounted className="pt-4">
          <ComplexAssignTab key={`c-${location.key}`} prefill={prefill} onCancel={cancel} />
        </TabsPanel>
      </Tabs>
    </div>
  );
}
