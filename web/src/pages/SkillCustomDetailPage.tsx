import { useParams } from 'react-router';
import { useIntl } from 'react-intl';
import { Puzzle } from 'lucide-react';
import { Page, PageHeader, Card, EmptyState } from '@/components/ui';
import { CustomSkillDetail } from '@/components/skills/CustomSkillDetail';

/** `/skills/custom/:id` — a self-built skill's detail (V13 / T13.2 / T13.4). */
export function SkillCustomDetailPage() {
  const { id } = useParams<{ id: string }>();
  const intl = useIntl();
  if (!id) {
    return (
      <Page>
        <PageHeader icon={Puzzle} title={intl.formatMessage({ id: 'skills.custom.title' })} />
        <Card>
          <EmptyState icon={Puzzle} title={intl.formatMessage({ id: 'skills.custom.notFound' })} />
        </Card>
      </Page>
    );
  }
  return <CustomSkillDetail id={id} />;
}
