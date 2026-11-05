import { useEffect } from 'react';
import { useIntl } from 'react-intl';
import { useLocation, useNavigate, useSearchParams } from 'react-router';
import { Settings, Mic, Zap, KeyRound, SlidersHorizontal, Building2, Mail } from 'lucide-react';
import { SettingsShell, SettingsTab, type SettingsNavGroup } from '@/components/mds';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { isNewFeature } from '@/apps/registry';
import { GeneralTab } from '@/components/settings/sections/GeneralTab';
import { AccountTab } from '@/components/settings/sections/AccountTab';
import { VoiceTab } from '@/components/settings/sections/VoiceTab';
import { ProactiveTab } from '@/components/settings/sections/ProactiveTab';
import { AdvancedSettings } from '@/components/settings/AdvancedSettings';
import { InvitationTab } from '@/components/settings/sections/InvitationTab';
import { CompanySettings } from '@/components/settings/CompanySettings';
import { ADVANCED_SETTINGS_PATH, LEGACY_ADVANCED_TABS } from '@/components/settings/advanced-settings-catalog';
import { RestartRequiredBanner } from '@/components/settings/RestartRequiredBanner';

const INVITATION_NEW_IN = '1.80.0';
const SETTINGS_PATH = '/app/system/settings';
const COMPANY_PATH = `${SETTINGS_PATH}/company`;
const VALID_TABS = ['general', 'account', 'voice', 'proactive', 'advanced', 'company', 'invitation'] as const;
type TabId = (typeof VALID_TABS)[number];

/** Common settings stay short; uncommon settings share one searchable Admin page. */
export function SettingsPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const [searchParams] = useSearchParams();
  const tabParam = searchParams.get('tab');
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const version = useSystemStore((s) => s.status?.version ?? null);
  const requested = pathname === ADVANCED_SETTINGS_PATH ? 'advanced' : pathname === COMPANY_PATH ? 'company' : tabParam;
  const tabAllowed = (tab: string | null): tab is TabId =>
    (VALID_TABS as readonly (string | null)[]).includes(tab) && (isAdmin || (tab !== 'advanced' && tab !== 'company' && tab !== 'invitation'));
  const activeTab: TabId = tabAllowed(requested) ? requested : 'general';

  useEffect(() => {
    if (tabParam === 'cron') navigate('/routines', { replace: true });
    else if (tabParam === 'update') navigate('/app/system/updates', { replace: true });
    else if (isAdmin && (LEGACY_ADVANCED_TABS as readonly (string | null)[]).includes(tabParam)) {
      navigate(`${ADVANCED_SETTINGS_PATH}#${tabParam}`, { replace: true });
    } else if (isAdmin && tabParam === 'advanced' && pathname !== ADVANCED_SETTINGS_PATH) {
      navigate(ADVANCED_SETTINGS_PATH, { replace: true });
    } else if (isAdmin && tabParam === 'company' && pathname !== COMPANY_PATH) {
      navigate(COMPANY_PATH, { replace: true });
    }
  }, [tabParam, pathname, isAdmin, navigate]);

  const setTab = (next: string) => {
    if (!tabAllowed(next)) return;
    navigate(next === 'advanced' ? ADVANCED_SETTINGS_PATH : next === 'company' ? COMPANY_PATH : `${SETTINGS_PATH}?tab=${next}`);
  };
  const t = (id: string) => intl.formatMessage({ id });
  const newLabel = (id: string) => isNewFeature('1.80.0', version) ? (
    <span className="flex items-center gap-1.5">
      {t(id)}
      <span className="rounded border border-surface-border px-1.5 py-0.5 text-xs text-muted-foreground">{t('nav.badge.new')}</span>
    </span>
  ) : t(id);
  const navGroups: SettingsNavGroup[] = [
    {
      label: t('settings.everyday'),
      items: [
        { value: 'general', label: t('settings.general'), icon: Settings },
        { value: 'account', label: t('settings.account'), icon: KeyRound },
        ...(isAdmin ? [{
          value: 'invitation',
          label: isNewFeature(INVITATION_NEW_IN, version) ? (
            <span className="flex items-center gap-1.5">
              {t('invitation.title')}
              <span className="rounded border border-surface-border px-1.5 py-0.5 text-xs text-muted-foreground">{t('nav.badge.new')}</span>
            </span>
          ) : t('invitation.title'),
          icon: Mail,
        }] : []),
        { value: 'voice', label: t('settings.voice'), icon: Mic },
        { value: 'proactive', label: t('settings.proactive'), icon: Zap },
        ...(isAdmin ? [{ value: 'company', label: newLabel('companyInfo.title'), icon: Building2 }] : []),
      ],
    },
    ...(isAdmin ? [{
      label: t('settings.advanced'),
      items: [{ value: 'advanced', label: newLabel('advancedSettings.title'), icon: SlidersHorizontal }],
    }] : []),
  ];

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="px-4 pt-3 empty:hidden"><RestartRequiredBanner /></div>
      <SettingsShell value={activeTab} onValueChange={setTab} groups={navGroups}>
        <SettingsTab value="general" title={t('settings.general')}><GeneralTab /></SettingsTab>
        <SettingsTab value="account" title={t('settings.account.title')}><AccountTab /></SettingsTab>
        {isAdmin && <SettingsTab value="invitation" title={t('invitation.title')}><InvitationTab /></SettingsTab>}
        <SettingsTab value="voice" title={t('voice.title')}><VoiceTab /></SettingsTab>
        <SettingsTab value="proactive" title={t('proactive.title')}><ProactiveTab /></SettingsTab>
        {isAdmin && <SettingsTab value="company" title={t('companyInfo.title')} description={t('companyInfo.description')}><CompanySettings /></SettingsTab>}
        {isAdmin && <SettingsTab value="advanced" title={t('advancedSettings.title')} description={t('advancedSettings.description')}><AdvancedSettings /></SettingsTab>}
      </SettingsShell>
    </div>
  );
}
