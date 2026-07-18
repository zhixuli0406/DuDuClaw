import { useParams, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { Puzzle } from 'lucide-react';
import { BreadcrumbHeader, CollectionPageState, Button } from '@/components/mds';
import { CustomSkillDetail } from '@/components/skills/CustomSkillDetail';

/** `/skills/custom/:id` — a self-built skill's detail (V13 / T13.2 / T13.4). */
export function SkillCustomDetailPage() {
  const { id } = useParams<{ id: string }>();
  const intl = useIntl();
  const navigate = useNavigate();
  if (!id) {
    return (
      <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
        <BreadcrumbHeader
          segments={[
            { label: intl.formatMessage({ id: 'nav.skills' }), onClick: () => navigate('/skills') },
            { label: intl.formatMessage({ id: 'skills.custom.title' }) },
          ]}
        />
        <CollectionPageState
          state="empty"
          icon={Puzzle}
          title={intl.formatMessage({ id: 'skills.custom.notFound' })}
          action={
            <Button variant="outline" size="sm" onClick={() => navigate('/skills')}>
              {intl.formatMessage({ id: 'skills.custom.backToList' })}
            </Button>
          }
        />
      </div>
    );
  }
  return <CustomSkillDetail id={id} />;
}
