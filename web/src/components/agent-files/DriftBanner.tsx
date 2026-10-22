import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { Button } from '@/components/mds';

export interface DriftBannerProps {
  /** The last approved version, when it is still kept (restore needs it). */
  acceptedVersionId: string | null;
  /** This person may change the file (its owner or an admin). */
  canEdit: boolean;
  busy?: boolean;
  /** Restore the last approved version (runs the whole check). */
  onRestore: () => void;
  /** Check the file as it is now and accept it as the approved version. */
  onAccept: () => void;
}

/**
 * DriftBanner — the file was changed outside the dashboard and no longer
 * matches the version last approved here. Offers the two ways back: restore
 * the approved version, or check the current text and accept it. Both go
 * through the same check as any other save.
 */
export function DriftBanner({ acceptedVersionId, canEdit, busy = false, onRestore, onAccept }: DriftBannerProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <section
      role="status"
      aria-label={t('agentFiles.drift.title')}
      className="space-y-2 rounded-lg border border-warning/30 bg-warning/10 p-3 text-sm"
      data-testid="drift-banner"
    >
      <p className="flex items-start gap-2 font-medium text-foreground">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
        {t('agentFiles.drift.title')}
      </p>
      <p className="text-muted-foreground">{t('agentFiles.drift.body')}</p>
      {!acceptedVersionId && <p className="text-xs text-muted-foreground">{t('agentFiles.drift.noApprovedVersion')}</p>}
      {canEdit ? (
        <div className="flex flex-wrap gap-2">
          {acceptedVersionId && (
            <Button size="sm" variant="outline" onClick={onRestore} disabled={busy}>
              {t('agentFiles.drift.restore')}
            </Button>
          )}
          <Button size="sm" variant="outline" onClick={onAccept} disabled={busy}>
            {t('agentFiles.drift.accept')}
          </Button>
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">{t('agentFiles.drift.ownerOnly')}</p>
      )}
    </section>
  );
}
