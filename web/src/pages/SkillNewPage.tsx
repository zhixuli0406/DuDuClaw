import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { BreadcrumbHeader } from '@/components/mds';
import { SkillWizard } from '@/components/skills/SkillWizard';

/** `/skills/new` — the self-serve skill wizard (V13 / §5.3 / T13.1). */
export function SkillNewPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <BreadcrumbHeader
        segments={[
          { label: intl.formatMessage({ id: 'nav.skills' }), onClick: () => navigate('/skills') },
          { label: intl.formatMessage({ id: 'skills.new.title' }) },
        ]}
      />
      <div className="mx-auto w-full max-w-4xl px-5 py-6 md:px-8 md:py-8">
        <SkillWizard />
      </div>
    </div>
  );
}
