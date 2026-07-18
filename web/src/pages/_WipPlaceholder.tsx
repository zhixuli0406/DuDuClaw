import type { ComponentType } from 'react';
import { useIntl } from 'react-intl';
import { Hammer } from 'lucide-react';
import { PageHeader, Empty } from '@/components/mds';

/**
 * WipPlaceholder — the "建置中" stand-in for v2 routes whose real surface ships
 * in a later wave. Keeps the route table complete and navigable now; each is
 * replaced in place later.
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
    <div className="flex min-h-0 flex-1 flex-col">
      <PageHeader>
        <span className="text-sm font-medium">{intl.formatMessage({ id: titleId })}</span>
      </PageHeader>
      <div className="flex flex-1 items-center justify-center p-6">
        <Empty
          icon={Icon}
          title={intl.formatMessage({ id: 'wip.title' })}
          description={intl.formatMessage({ id: hintId })}
        />
      </div>
    </div>
  );
}
