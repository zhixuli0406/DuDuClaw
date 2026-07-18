import { useEffect } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
import {
  Settings,
  Container,
  HeartPulse,
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
} from 'lucide-react';
import {
  SettingsShell,
  SettingsTab,
  type SettingsNavGroup,
} from '@/components/mds';
import { GeneralTab } from '@/components/settings/sections/GeneralTab';
import { AccountTab } from '@/components/settings/sections/AccountTab';
import { SystemTab } from '@/components/settings/sections/SystemTab';
import { ContainerTab } from '@/components/settings/sections/ContainerTab';
import { HeartbeatTab } from '@/components/settings/sections/HeartbeatTab';
import { VoiceTab } from '@/components/settings/sections/VoiceTab';
import { ProactiveTab } from '@/components/settings/sections/ProactiveTab';
import { AutopilotTab } from '@/components/settings/sections/AutopilotTab';
import { SkillSynthesisTab } from '@/components/settings/sections/SkillSynthesisTab';
import { RedactionTab } from '@/components/settings/sections/RedactionTab';
import { DoctorTab } from '@/components/settings/sections/DoctorTab';
import { UpdateTab } from '@/components/settings/sections/UpdateTab';
import { BrowserTab } from '@/components/settings/sections/BrowserTab';

/** Settings sub-tab whitelist (spec §5.3 式3). `?tab=` is validated against this
 *  set; unknown values fall back to `general`. */
const VALID_TABS = [
  'general', 'account', 'system', 'container', 'heartbeat', 'voice',
  'proactive', 'autopilot', 'skillSynthesis', 'redaction', 'doctor', 'update', 'browser',
] as const;
type TabId = (typeof VALID_TABS)[number];

/**
 * SettingsPage (WP4.2) — the system settings surface rebuilt as a Multica
 * Settings-式 shell (spec §5.3 式3): a grouped left rail (常用 / 進階, vertical
 * ≥md, horizontal-scroll on mobile) driving a `max-w-3xl` scrolling content pane
 * of 13 section panels. Replaces the former Calm Glass Page/PageHeader/Tabs +
 * ChevronDown "advanced" disclosure; the `?tab=` whitelist and the legacy
 * branding/cron deep-link redirects are preserved.
 */
export function SettingsPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const tabParam = searchParams.get('tab');

  // Legacy deep-link redirects (bookmarks / older links) instead of a blank tab:
  //  - branding moved to /manage/distributors (R5)
  //  - the cron/排程任務 settings tab was unified into the /routines page.
  useEffect(() => {
    if (tabParam === 'branding') {
      navigate('/manage/distributors?tab=branding', { replace: true });
    } else if (tabParam === 'cron') {
      navigate('/routines', { replace: true });
    }
  }, [tabParam, navigate]);

  const activeTab: TabId = (VALID_TABS as readonly string[]).includes(tabParam ?? '')
    ? (tabParam as TabId)
    : 'general';
  const setTab = (next: string) => {
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set('tab', next);
    setSearchParams(nextParams, { replace: true });
  };

  const t = (id: string) => intl.formatMessage({ id });

  // Rail groups mirror the former everyday / advanced split.
  const navGroups: SettingsNavGroup[] = [
    {
      label: t('settings.everyday'),
      items: [
        { value: 'general', label: t('settings.general'), icon: Settings },
        { value: 'account', label: t('settings.account'), icon: KeyRound },
        { value: 'voice', label: t('settings.voice'), icon: Mic },
        { value: 'proactive', label: t('settings.proactive'), icon: Zap },
        { value: 'update', label: t('settings.update'), icon: Download },
      ],
    },
    {
      label: t('settings.advanced'),
      items: [
        { value: 'system', label: t('settings.system'), icon: Server },
        { value: 'container', label: t('settings.container'), icon: Container },
        { value: 'heartbeat', label: t('settings.heartbeat'), icon: HeartPulse },
        { value: 'autopilot', label: t('settings.autopilot'), icon: Workflow },
        { value: 'skillSynthesis', label: t('settings.skillSynthesis'), icon: Sparkles },
        { value: 'redaction', label: t('settings.redaction'), icon: EyeOff },
        { value: 'doctor', label: t('settings.doctor'), icon: Stethoscope },
        { value: 'browser', label: t('settings.browser'), icon: Globe },
      ],
    },
  ];

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <SettingsShell value={activeTab} onValueChange={setTab} groups={navGroups}>
        <SettingsTab value="general" title={t('settings.general')}>
          <GeneralTab />
        </SettingsTab>
        <SettingsTab value="account" title={t('settings.account.title')}>
          <AccountTab />
        </SettingsTab>
        <SettingsTab value="system" title={t('settings.system')} description={t('settings.system.desc')}>
          <SystemTab />
        </SettingsTab>
        <SettingsTab value="container" title={t('settings.container')} description={t('settings.container.desc')}>
          <ContainerTab />
        </SettingsTab>
        <SettingsTab value="heartbeat" title={t('settings.heartbeat')} description={t('settings.heartbeat.desc')}>
          <HeartbeatTab />
        </SettingsTab>
        <SettingsTab value="voice" title={t('voice.title')}>
          <VoiceTab />
        </SettingsTab>
        <SettingsTab value="proactive" title={t('proactive.title')}>
          <ProactiveTab />
        </SettingsTab>
        <SettingsTab value="autopilot" title={t('settings.autopilot')} description={t('settings.autopilot.desc')}>
          <AutopilotTab />
        </SettingsTab>
        <SettingsTab value="skillSynthesis" title={t('settings.skillSynthesis')} description={t('skillSynthesis.desc')}>
          <SkillSynthesisTab />
        </SettingsTab>
        <SettingsTab value="redaction" title={t('settings.redaction')}>
          <RedactionTab />
        </SettingsTab>
        <SettingsTab value="doctor" title={t('settings.doctor')} description={t('settings.doctor.desc')}>
          <DoctorTab />
        </SettingsTab>
        <SettingsTab value="update" title={t('settings.update')}>
          <UpdateTab />
        </SettingsTab>
        <SettingsTab value="browser" title={t('settings.browser')} description={t('settings.browser.desc')}>
          <BrowserTab />
        </SettingsTab>
      </SettingsShell>
    </div>
  );
}
