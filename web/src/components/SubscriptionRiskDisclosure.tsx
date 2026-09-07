import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { Checkbox } from '@/components/mds';

/**
 * §1-1 risk disclosure (TODO-ai-runtimes-2026-09.md — decision 1B): every
 * one-click "login with your subscription" flow (`CliLoginModal`,
 * `SubscriptionSetupWizard`, and by extension `RuntimeSetupCard`, which only
 * ever opens one of those two) must show this notice and require an explicit
 * checkbox before the actual login/device-code flow starts. Anthropic and
 * Google have blocked third-party use of consumer-subscription tokens
 * server-side since 2026-03 and suspended accounts over it; OpenAI's policy
 * is unspecified. The API-key path (accounts.add / AddAccountDialog) is
 * unaffected and stays the default recommendation — this notice only gates
 * the subscription-token login surfaces.
 *
 * Shared by every surface that gates a subscription login so the copy and
 * behavior can't drift between them.
 */
export function SubscriptionRiskDisclosure({
  checked,
  onCheckedChange,
}: {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  const intl = useIntl();
  return (
    <div className="space-y-3 rounded-lg border border-warning/30 bg-warning/10 p-3">
      <div className="flex items-start gap-2 text-xs text-warning">
        <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
        <p>{intl.formatMessage({ id: 'subscriptionRisk.notice' })}</p>
      </div>
      <label className="flex items-start gap-2 text-xs text-foreground">
        <Checkbox
          checked={checked}
          onCheckedChange={onCheckedChange}
          aria-label={intl.formatMessage({ id: 'subscriptionRisk.checkbox' })}
        />
        <span>{intl.formatMessage({ id: 'subscriptionRisk.checkbox' })}</span>
      </label>
    </div>
  );
}
