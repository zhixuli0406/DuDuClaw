import { useIntl } from 'react-intl';
import { AccountTab } from '@/components/settings/sections/AccountTab';

export function AccountPage() {
  const intl = useIntl();
  return (
    <div className="mx-auto max-w-4xl space-y-6 p-6">
      <div>
        <h1 className="text-2xl font-semibold">{intl.formatMessage({ id: 'nav.account' })}</h1>
        <p className="mt-2 text-sm text-muted-foreground">{intl.formatMessage({ id: 'nav.account.desc' })}</p>
      </div>
      <AccountTab />
    </div>
  );
}
