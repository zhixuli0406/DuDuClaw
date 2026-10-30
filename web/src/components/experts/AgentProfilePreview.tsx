import { useIntl } from 'react-intl';
import type { AgentProfilePreview as ProfilePreview } from '@/lib/api';

/** Imported content is displayed as text, never rendered as trusted markup. */
export function AgentProfilePreview({ profiles }: { profiles: ProfilePreview[] }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <section className="space-y-4" aria-label={t('agentProfile.import.review')}>
      <p className="text-sm">{t('agentProfile.import.review')}</p>
      {profiles.map((profile) => (
        <div key={profile.id} className="space-y-3 rounded-lg border border-surface-border p-3">
          <h3 className="font-medium">{profile.name || profile.id}</h3>
          {profile.name && <p className="text-xs">{profile.id}</p>}
          {profile.metadata?.description && <p className="whitespace-pre-wrap break-words text-sm">{profile.metadata.description}</p>}
          {!!profile.unmapped_fields?.length && (
            <section>
              <h4 className="text-sm font-medium">{t('agentProfile.import.unmapped')}</h4>
              {profile.unmapped_fields.map(([field, reason], index) => (
                <p key={`${field}-${index}`} className="whitespace-pre-wrap break-words text-xs">{field}: {reason}</p>
              ))}
            </section>
          )}
          {(['persona', 'user', 'rules', 'contract'] as const).map((section) => (
            <section key={section}>
              <h4 className="text-sm font-medium">{t(`agentProfile.section.${section}`)}</h4>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words text-xs">{section === 'persona' ? [profile.persona.soul, profile.persona.identity].filter(Boolean).join('\n\n') : profile[section]}</pre>
            </section>
          ))}
          {profile.settings && Object.keys(profile.settings).length > 0 && (
            <section>
              <h4 className="text-sm font-medium">{t('agentFiles.file.agent_toml')}</h4>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words text-xs">{JSON.stringify(profile.settings, null, 2)}</pre>
            </section>
          )}
          <h4 className="text-sm font-medium">{t('agentProfile.import.guard')}</h4>
          {profile.input_guard.map((check) => (
            <p key={check.section} className="text-xs">{check.section}: {check.blocked || check.matched_rules.length ? `${t('agentProfile.import.flagged')} (${check.score}): ${check.matched_rules.join(', ')}` : t('agentProfile.import.clean')}</p>
          ))}
        </div>
      ))}
    </section>
  );
}
