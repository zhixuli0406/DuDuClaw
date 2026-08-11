import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { ArrowRight } from 'lucide-react';
import { Card, CardHeader, CardTitle, Button } from '@/components/mds';

/**
 * NeedsMeRow — the customizable "需要我" widget on Home (WP1.5, spec §5.5
 * report style).
 *
 * W2-5: collapsed from a per-row list (approval / blocked / budget /
 * failed-run titles, same shape as `NeedsAttentionList`) down to a single
 * summary + "前往收件匣" pointer. Once the fixed "overview first" section
 * (W2-1's `HealthOverview`, always rendered above the customizable widget
 * grid) started listing the exact same merged "needs me" items in full via
 * `NeedsAttentionList`, this widget re-listing its own top-4 slice of those
 * SAME rows further down the same page was pure duplication — two lists of
 * the same titles, one scroll apart. A summary card is the minimal fix that
 * keeps the widget meaningfully reorderable/hideable in the customizable
 * grid (unlike the fixed `HealthOverview`, which can't be moved or hidden)
 * without re-listing content the user already saw seconds earlier.
 *
 * Empty → renders nothing (WP1.5 拍板: "空則不顯示"), keeping the home canvas
 * quiet when there is nothing waiting on the user.
 */
export interface NeedsMeRowProps {
  /** Total count across the merged "needs me" sources (approvals / blocked /
   *  needs_human / budget / failed runs). */
  total: number;
}

export function NeedsMeRow({ total }: NeedsMeRowProps) {
  const intl = useIntl();
  const navigate = useNavigate();

  // 拍板: empty state is silence, not a friendly card.
  if (total === 0) return null;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{intl.formatMessage({ id: 'home.inbox.title' })}</CardTitle>
      </CardHeader>
      <div className="flex flex-wrap items-center justify-between gap-3 px-4 pb-4">
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'home.inbox.summary' }, { count: total })}
        </p>
        <Button variant="outline" size="sm" onClick={() => navigate('/inbox')}>
          {intl.formatMessage({ id: 'home.inbox.viewAll' })}
          <ArrowRight />
        </Button>
      </div>
    </Card>
  );
}
