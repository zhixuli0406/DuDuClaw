import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { cn } from '@/lib/utils';
import { Page, PageHeader, Tabs } from '@/components/ui';
import {
  Settings,
  Container,
  HeartPulse,
  Clock,
  Stethoscope,
  Mic,
  Zap,
  Workflow,
  Globe,
  Server,
  Sparkles,
  EyeOff,
  Download,
  KeyRound,
  ChevronDown,
} from 'lucide-react';
import { GeneralTab } from '@/components/settings/sections/GeneralTab';
import { AccountTab } from '@/components/settings/sections/AccountTab';
import { SystemTab } from '@/components/settings/sections/SystemTab';
import { ContainerTab } from '@/components/settings/sections/ContainerTab';
import { HeartbeatTab } from '@/components/settings/sections/HeartbeatTab';
import { CronTab } from '@/components/settings/sections/CronTab';
import { VoiceTab } from '@/components/settings/sections/VoiceTab';
import { ProactiveTab } from '@/components/settings/sections/ProactiveTab';
import { AutopilotTab } from '@/components/settings/sections/AutopilotTab';
import { SkillSynthesisTab } from '@/components/settings/sections/SkillSynthesisTab';
import { RedactionTab } from '@/components/settings/sections/RedactionTab';
import { DoctorTab } from '@/components/settings/sections/DoctorTab';
import { UpdateTab } from '@/components/settings/sections/UpdateTab';
import { BrowserTab } from '@/components/settings/sections/BrowserTab';

type TabId = 'general' | 'account' | 'system' | 'container' | 'heartbeat' | 'cron' | 'voice' | 'proactive' | 'autopilot' | 'skillSynthesis' | 'redaction' | 'doctor' | 'update' | 'browser';

export function SettingsPage() {
  const intl = useIntl();
  const [searchParams] = useSearchParams();
  const initialTab = (searchParams.get('tab') as TabId) || 'general';
  const [activeTab, setActiveTab] = useState<TabId>(initialTab);
  const [showAdvanced, setShowAdvanced] = useState(false);

  const TAB_META: Record<TabId, { label: string; icon: React.ComponentType<{ className?: string }> }> = {
    general: { label: intl.formatMessage({ id: 'settings.general' }), icon: Settings },
    account: { label: intl.formatMessage({ id: 'settings.account' }), icon: KeyRound },
    system: { label: intl.formatMessage({ id: 'settings.system' }), icon: Server },
    container: { label: intl.formatMessage({ id: 'settings.container' }), icon: Container },
    heartbeat: { label: intl.formatMessage({ id: 'settings.heartbeat' }), icon: HeartPulse },
    cron: { label: intl.formatMessage({ id: 'settings.cron' }), icon: Clock },
    voice: { label: intl.formatMessage({ id: 'settings.voice' }), icon: Mic },
    proactive: { label: intl.formatMessage({ id: 'settings.proactive' }), icon: Zap },
    autopilot: { label: intl.formatMessage({ id: 'settings.autopilot' }), icon: Workflow },
    skillSynthesis: { label: intl.formatMessage({ id: 'settings.skillSynthesis' }), icon: Sparkles },
    redaction: { label: intl.formatMessage({ id: 'settings.redaction' }), icon: EyeOff },
    doctor: { label: intl.formatMessage({ id: 'settings.doctor' }), icon: Stethoscope },
    update: { label: intl.formatMessage({ id: 'settings.update' }), icon: Download },
    browser: { label: intl.formatMessage({ id: 'settings.browser' }), icon: Globe },
  };
  // Non-engineers only need a handful of these day-to-day; the rest are
  // system/engineering knobs tucked behind an "Advanced" disclosure so the
  // default surface reads like a consumer app, not an ops console.
  const EVERYDAY: TabId[] = ['general', 'account', 'voice', 'proactive', 'update'];
  const ADVANCED: TabId[] = ['system', 'container', 'heartbeat', 'cron', 'autopilot', 'skillSynthesis', 'redaction', 'doctor', 'browser'];
  const toItems = (ids: TabId[]) => ids.map((id) => ({ id, ...TAB_META[id] }));
  const activeIsAdvanced = ADVANCED.includes(activeTab);

  return (
    <Page wide>
      <PageHeader
        icon={Settings}
        title={intl.formatMessage({ id: 'nav.settings' })}
        subtitle={intl.formatMessage({ id: 'settings.title' })}
      />

      <Tabs
        items={toItems(EVERYDAY)}
        value={activeTab}
        onChange={(id) => setActiveTab(id as TabId)}
      />

      <button
        type="button"
        onClick={() => setShowAdvanced((v) => !v)}
        aria-expanded={showAdvanced || activeIsAdvanced}
        className="mt-3 flex items-center gap-1.5 text-xs font-medium text-stone-500 transition-colors hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200"
      >
        <ChevronDown
          className={cn('h-3.5 w-3.5 transition-transform', !(showAdvanced || activeIsAdvanced) && '-rotate-90')}
        />
        {intl.formatMessage({ id: 'settings.advanced' })}
        <span className="font-normal text-stone-400 dark:text-stone-500">
          · {intl.formatMessage({ id: 'settings.advanced.hint' })}
        </span>
      </button>
      {(showAdvanced || activeIsAdvanced) && (
        <div className="mt-1.5">
          <Tabs
            items={toItems(ADVANCED)}
            value={activeTab}
            onChange={(id) => setActiveTab(id as TabId)}
          />
        </div>
      )}

      {activeTab === 'general' && <GeneralTab />}
      {activeTab === 'account' && <AccountTab />}
      {activeTab === 'system' && <SystemTab />}
      {activeTab === 'container' && <ContainerTab />}
      {activeTab === 'heartbeat' && <HeartbeatTab />}
      {activeTab === 'cron' && <CronTab />}
      {activeTab === 'voice' && <VoiceTab />}
      {activeTab === 'proactive' && <ProactiveTab />}
      {activeTab === 'autopilot' && <AutopilotTab />}
      {activeTab === 'skillSynthesis' && <SkillSynthesisTab />}
      {activeTab === 'redaction' && <RedactionTab />}
      {activeTab === 'doctor' && <DoctorTab />}
      {activeTab === 'update' && <UpdateTab />}
      {activeTab === 'browser' && <BrowserTab />}
    </Page>
  );
}
