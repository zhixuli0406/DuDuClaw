import { useIntl } from 'react-intl';

export const GOOGLE_PRIVACY_URL = 'https://dudustudio.monster/duduclaw/privacy/';

export function GoogleDataDisclosure({
  confirmed,
  onConfirmedChange,
}: {
  confirmed: boolean;
  onConfirmedChange: (confirmed: boolean) => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  return (
    <section className="space-y-2 rounded-lg border border-warning/40 bg-warning/5 p-3">
      <h3 className="text-sm font-medium text-foreground">{t('googleDataDisclosure.title')}</h3>
      <p className="text-xs text-muted-foreground">{t('googleDataDisclosure.access')}</p>
      <p className="text-xs text-muted-foreground">{t('googleDataDisclosure.storage')}</p>
      <p className="text-xs text-muted-foreground">{t('googleDataDisclosure.training')}</p>
      <a
        href={GOOGLE_PRIVACY_URL}
        target="_blank"
        rel="noreferrer"
        className="inline-flex text-xs text-brand underline underline-offset-2"
      >
        {t('googleDataDisclosure.privacy')}
      </a>
      <label className="flex items-start gap-2 text-sm text-foreground">
        <input
          type="checkbox"
          className="mt-0.5"
          checked={confirmed}
          onChange={(event) => onConfirmedChange(event.target.checked)}
        />
        <span>{t('googleDataDisclosure.confirm')}</span>
      </label>
    </section>
  );
}
