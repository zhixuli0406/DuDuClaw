import { useIntl } from 'react-intl';
import type { AgentProfilePreview as ProfilePreview } from '@/lib/api';

/** Imported content is displayed as text, never rendered as trusted markup. */
export function AgentProfilePreview({ profiles, plan }: {
  profiles: ProfilePreview[];
  plan?: Record<string, unknown>;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <section className="space-y-4" aria-label={t('agentProfile.import.review')}>
      <p className="text-sm">{t('agentProfile.import.review')}</p>
      {profiles.map((profile) => (
        <div key={profile.id} className="space-y-3 rounded-lg border border-surface-border p-3">
          <h3 className="font-medium">{profile.name || profile.id}</h3>
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
            <dt>{t('agentProfile.import.id')}</dt>
            <dd className="break-words">{profile.id}</dd>
            <dt>{t('agentProfile.import.name')}</dt>
            <dd className="break-words">{profile.name || profile.id}</dd>
            <dt>{t('agentProfile.import.model')}</dt>
            <dd className="break-words">{profile.model || t('agentProfile.import.defaultModel')}</dd>
            <dt>{t('agentProfile.import.parent')}</dt>
            <dd className="break-words">{profile.reports_to || t('agentProfile.import.root')}</dd>
            <dt>{t('agentProfile.import.tools')}</dt>
            <dd className="whitespace-pre-wrap break-words">{profile.tools?.length ? profile.tools.join(', ') : t('agentProfile.import.defaultTools')}</dd>
            <dt>{t('agentProfile.import.deniedTools')}</dt>
            <dd className="whitespace-pre-wrap break-words">{profile.disallowed_tools?.length ? profile.disallowed_tools.join(', ') : t('agentProfile.import.noDeniedTools')}</dd>
          </dl>
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
          {!!profile.files?.length && (
            <section>
              <h4 className="text-sm font-medium">{t('agentProfile.import.files')}</h4>
              <ul className="max-h-64 space-y-2 overflow-auto text-xs">
                {profile.files.map((file) => (
                  <li key={file.path}>
                    <p className="whitespace-pre-wrap break-words">{file.path}</p>
                    <code className="break-all">SHA-256: {file.sha256}</code>
                  </li>
                ))}
              </ul>
            </section>
          )}
          <h4 className="text-sm font-medium">{t('agentProfile.import.guard')}</h4>
          {profile.input_guard.map((check) => (
            <p key={check.section} className="text-xs">{check.section}: {check.blocked || check.matched_rules.length ? `${t('agentProfile.import.flagged')} (${check.score}): ${check.matched_rules.join(', ')}` : t('agentProfile.import.clean')}</p>
          ))}
        </div>
      ))}
      {plan && (
        <section>
          <h3 className="text-sm font-medium">{t('agentProfile.import.plan')}</h3>
          <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words text-xs">{JSON.stringify(plan, null, 2)}</pre>
        </section>
      )}
    </section>
  );
}
