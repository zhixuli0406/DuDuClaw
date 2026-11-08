import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link, useLocation } from 'react-router';
import { ChevronDown, Search } from 'lucide-react';
import { Input, Card, CardHeader, CardTitle, CardDescription, CardContent } from '@/components/mds';
import { useAuthStore } from '@/stores/auth-store';
import { SystemTab } from './sections/SystemTab';
import { ContainerTab } from './sections/ContainerTab';
import { HeartbeatTab } from './sections/HeartbeatTab';
import { AutopilotTab } from './sections/AutopilotTab';
import { SkillSynthesisTab } from './sections/SkillSynthesisTab';
import { RedactionTab } from './sections/RedactionTab';
import { EmbeddingSettingsCard } from '@/components/knowledge/EmbeddingSettingsCard';
import { DoctorTab } from './sections/DoctorTab';
import { BrowserTab } from './sections/BrowserTab';
import { ExternalStatusSettings } from './sections/ExternalStatusSettings';
import { AutomationTab } from './sections/AutomationTab';
import { DelegationTab } from './sections/DelegationTab';
import { CalibrationTab } from './sections/CalibrationTab';
import { ConfigRawTab } from './sections/ConfigRawTab';
import { AdvancedGeneralPreferences } from './sections/AdvancedGeneralPreferences';
import { ADVANCED_GROUPS, ADVANCED_SETTINGS, filterAdvancedSettings } from './advanced-settings-catalog';

function SettingPanel({ id }: { id: string }) {
  const intl = useIntl();
  switch (id) {
    case 'system': return <SystemTab section="system" />;
    case 'security': return <SystemTab section="security" />;
    case 'utility': return <SystemTab section="models" />;
    case 'rotation': return <AdvancedGeneralPreferences field="rotation" />;
    case 'logging': return <AdvancedGeneralPreferences field="logging" />;
    case 'container': return <ContainerTab />;
    case 'heartbeat': return <HeartbeatTab />;
    case 'autopilot': return <AutopilotTab />;
    case 'skillSynthesis': return <SkillSynthesisTab />;
    case 'redaction': return <RedactionTab />;
    case 'knowledgeSearch': return <EmbeddingSettingsCard />;
    case 'doctor': return <DoctorTab />;
    case 'browser': return <BrowserTab />;
    case 'externalStatus': return <ExternalStatusSettings />;
    case 'automation': return <AutomationTab />;
    case 'delegation': return <DelegationTab />;
    case 'calibration': return <CalibrationTab />;
    case 'configRaw': return <ConfigRawTab />;
    case 'billing': return <Link className="text-primary underline underline-offset-4" to="/manage/billing">{intl.formatMessage({ id: 'advancedSettings.openBilling' })}</Link>;
    default: return null;
  }
}

export function AdvancedSettings() {
  const intl = useIntl();
  const { hash } = useLocation();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [query, setQuery] = useState('');
  const [opened, setOpened] = useState<Set<string>>(() => new Set());
  const t = (id: string) => intl.formatMessage({ id });

  useEffect(() => {
    const target = hash.slice(1);
    if (!ADVANCED_SETTINGS.some((entry) => entry.id === target)) return;
    setQuery('');
    setOpened((prev) => new Set([...prev, target]));
    const frame = requestAnimationFrame(() => document.getElementById(target)?.scrollIntoView?.({ block: 'start' }));
    return () => cancelAnimationFrame(frame);
  }, [hash]);

  // Keep direct component use fail-closed as well as the router's Admin guard.
  if (!isAdmin) return null;
  const results = filterAdvancedSettings(query, intl.messages);
  const visible = new Set(results.map((entry) => entry.id));
  return (
    <div className="space-y-8">
      <div className="relative">
        <Search aria-hidden="true" className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input type="search" className="h-11 pl-9" aria-label={t('advancedSettings.search')} placeholder={t('advancedSettings.search')} value={query} onChange={(event) => setQuery(event.target.value)} />
      </div>
      <p className="text-sm text-muted-foreground" role="status" aria-live="polite">
        {results.length === 0 ? t('advancedSettings.empty') : intl.formatMessage({ id: 'advancedSettings.results' }, { count: results.length })}
      </p>
      {ADVANCED_GROUPS.map((group) => (
        <section key={group} hidden={!results.some((entry) => entry.group === group)} aria-labelledby={`advanced-${group}`} className="space-y-4">
          <h2 id={`advanced-${group}`} className="text-base font-medium">{t(`advancedSettings.group.${group}`)}</h2>
          {ADVANCED_SETTINGS.filter((entry) => entry.group === group).map((entry) => (
            <div key={entry.id} id={entry.id} hidden={!visible.has(entry.id)} className="scroll-mt-6">
              <Card>
                <CardHeader>
                  <CardTitle>
                    <button type="button" className="flex min-h-11 w-full items-center justify-between gap-3 text-left" aria-expanded={opened.has(entry.id)} aria-controls={`advanced-panel-${entry.id}`} onClick={() => setOpened((prev) => {
                      const next = new Set(prev);
                      if (next.has(entry.id)) next.delete(entry.id); else next.add(entry.id);
                      return next;
                    })}>
                      {t(entry.title)}
                      <ChevronDown aria-hidden="true" className={`size-4 shrink-0 ${opened.has(entry.id) ? 'rotate-180' : ''}`} />
                    </button>
                  </CardTitle>
                  <CardDescription>{t(entry.description)}</CardDescription>
                </CardHeader>
                {opened.has(entry.id) && <CardContent id={`advanced-panel-${entry.id}`}><SettingPanel id={entry.id} /></CardContent>}
              </Card>
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}
