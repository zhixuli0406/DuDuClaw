import type { ComponentType } from 'react';
import { useIntl } from 'react-intl';
import { Hammer } from 'lucide-react';
import { Page, PageHeader, EmptyState } from '@/components/ui';

/**
 * WipPlaceholder — the "建置中" stand-in for v2 routes whose real surface ships
 * in a later wave (TaskDetail V5 / Growth V10 / SkillNew+Custom V13). Keeps the
 * route table complete and navigable now; each is replaced in place later.
 */
export function WipPlaceholder({
  titleId,
  hintId,
  icon: Icon = Hammer,
}: {
  titleId: string;
  hintId: string;
  icon?: ComponentType<{ className?: string }>;
}) {
  const intl = useIntl();
  return (
    <Page>
      <PageHeader title={intl.formatMessage({ id: titleId })} />
      <EmptyState
        icon={Icon}
        title={intl.formatMessage({ id: 'wip.title' })}
        hint={intl.formatMessage({ id: hintId })}
      />
    </Page>
  );
}
