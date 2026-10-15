import type { MouseEvent } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, ExternalLink } from 'lucide-react';
import { Checkbox } from '@/components/mds';
import { openExternal } from '@/lib/external-link';
import { cn } from '@/lib/utils';
import { ANTHROPIC_LEGAL_URL } from '@/lib/subscription-risk';

function PolicyLink({ labelId }: { labelId: string }) {
  const intl = useIntl();
  const onClick = (e: MouseEvent<HTMLAnchorElement>) => {
    e.preventDefault();
    openExternal(ANTHROPIC_LEGAL_URL);
  };
  return (
    <a
      href={ANTHROPIC_LEGAL_URL}
      target="_blank"
      rel="noreferrer"
      onClick={onClick}
      className="inline-flex items-center gap-1 font-medium text-brand underline underline-offset-2 hover:text-brand/80"
    >
      {intl.formatMessage({ id: labelId })}
      <ExternalLink className="h-3 w-3 shrink-0" aria-hidden />
    </a>
  );
}

/**
 * Subscription-login risk disclosure (owner decision 2026-10-16: the
 * subscription features stay, the disclosure must be accurate and shown at
 * every entry point). Every flow that starts or saves a consumer-subscription
 * credential (`CliLoginModal`, `SubscriptionSetupWizard`, the OAuth path of
 * `AddAccountDialog`) renders this and only enables its action once the box
 * is ticked. The gateway enforces the same rule: those RPCs refuse unless the
 * request carries `accept_subscription_risk: true`, and each acknowledgment is
 * written to the security audit log.
 *
 * Wording follows Anthropic's legal-and-compliance page and Consumer Terms.
 * It states policy, never a technical block (`claude -p` with an OAuth token
 * still works). The Google/OpenAI sentence is the pre-existing wording, kept
 * until a primary source is cited.
 */
export function SubscriptionRiskDisclosure({
  checked,
  onCheckedChange,
}: {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <div
      className="space-y-3 rounded-lg border border-warning/30 bg-warning/10 p-3"
      data-testid="subscription-risk-disclosure"
    >
      <p className="flex items-center gap-2 text-sm font-medium text-warning">
        <AlertTriangle className="h-4 w-4 shrink-0" aria-hidden />
        {t('subscriptionRisk.title')}
      </p>
      <div className="max-h-[45vh] space-y-2 overflow-y-auto pr-1 text-xs leading-relaxed text-foreground/90">
        <p>{t('subscriptionRisk.policy')}</p>
        <p>{t('subscriptionRisk.conflicts')}</p>
        <p>{t('subscriptionRisk.enforcement')}</p>
        <p className="text-muted-foreground">{t('subscriptionRisk.others')}</p>
        <p>{t('subscriptionRisk.recommendation')}</p>
        <p>
          <PolicyLink labelId="subscriptionRisk.link" />
        </p>
      </div>
      <label className="flex items-start gap-2 text-xs font-medium text-foreground">
        <Checkbox
          checked={checked}
          onCheckedChange={onCheckedChange}
          aria-label={t('subscriptionRisk.checkbox')}
        />
        <span>{t('subscriptionRisk.checkbox')}</span>
      </label>
    </div>
  );
}

/**
 * One-line form of the disclosure for cards and settings rows that lead to a
 * subscription credential without collecting one themselves. The action they
 * open still shows {@link SubscriptionRiskDisclosure} first.
 */
export function SubscriptionRiskNote({
  className,
  messageId = 'subscriptionRisk.short',
}: {
  className?: string;
  messageId?: string;
}) {
  const intl = useIntl();
  return (
    <p
      className={cn('flex items-start gap-1.5 text-xs text-warning', className)}
      data-testid="subscription-risk-note"
    >
      <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" aria-hidden />
      <span>
        {intl.formatMessage({ id: messageId })}{' '}
        <PolicyLink labelId="subscriptionRisk.learnMore" />
      </span>
    </p>
  );
}
