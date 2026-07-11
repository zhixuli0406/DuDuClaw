import { useIntl } from 'react-intl';
import { Wand2 } from 'lucide-react';
import { Page, PageHeader } from '@/components/ui';
import { SkillWizard } from '@/components/skills/SkillWizard';

/** `/skills/new` — the self-serve skill wizard (V13 / §5.6 / T13.1). */
export function SkillNewPage() {
  const intl = useIntl();
  return (
    <Page>
      <PageHeader
        icon={Wand2}
        title={intl.formatMessage({ id: 'skills.new.title' })}
        subtitle={intl.formatMessage({ id: 'skills.new.subtitle' })}
      />
      <SkillWizard />
    </Page>
  );
}
