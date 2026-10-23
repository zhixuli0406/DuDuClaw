import { useIntl } from 'react-intl';
import type { ExpertReimport } from '@/lib/api';
import { Button, Spinner } from '@/components/mds';

/**
 * ReimportChoice — the two explicit ways forward when an upload was
 * imported before: replace the earlier version (it goes to a backup an
 * administrator can restore) or keep it and import this one as a copy.
 * `slug` is the earlier import; omit it to offer only the copy (an
 * employee id clash with no earlier import to replace).
 */
export function ReimportChoice({
  slug,
  edited = false,
  busy,
  onChoose,
}: {
  slug?: string;
  edited?: boolean;
  busy: boolean;
  onChoose: (choice: ExpertReimport) => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  return (
    <div className="space-y-2">
      {slug && (
        <div className="rounded-lg border border-surface-border bg-background/60 p-2.5">
          <Button
            variant="brand"
            size="sm"
            disabled={busy}
            onClick={() => onChoose({ mode: 'replace', slug })}
          >
            {busy && <Spinner className="size-3.5" />}
            {t('experts.reimport.replace')}
          </Button>
          <p className="mt-1 text-muted-foreground">{t('experts.reimport.replaceDesc')}</p>
          {edited && <p className="mt-1 text-warning">{t('experts.reimport.edited')}</p>}
        </div>
      )}
      <div className="rounded-lg border border-surface-border bg-background/60 p-2.5">
        <Button variant="outline" size="sm" disabled={busy} onClick={() => onChoose({ mode: 'copy' })}>
          {t('experts.reimport.copy')}
        </Button>
        <p className="mt-1 text-muted-foreground">{t('experts.reimport.copyDesc')}</p>
      </div>
    </div>
  );
}
