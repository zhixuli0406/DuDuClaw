import { useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import {
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';

/**
 * EmbeddingDisclosureDialog — the one place a cloud provider can be turned
 * on. It names the vendor and the host that will receive document passages
 * (after de-identification) and needs an explicit tick before "Use" is
 * enabled. The gateway refuses a cloud choice without this acknowledgement.
 */
export function EmbeddingDisclosureDialog({
  open,
  vendor,
  host,
  model,
  redactionReady,
  redactionReason,
  customEndpoint,
  busy,
  onConfirm,
  onClose,
}: {
  open: boolean;
  vendor: string;
  host: string;
  model: string;
  redactionReady: boolean;
  redactionReason?: string;
  /** The configured endpoint is not the vendor's own: the API key goes there too. */
  customEndpoint: boolean;
  busy: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string>) => intl.formatMessage({ id }, values);
  const [ack, setAck] = useState(false);

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) {
          setAck(false);
          onClose();
        }
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('kb.embedding.disclosure.title', { vendor })}</DialogTitle>
          <DialogDescription>{t('kb.embedding.disclosure.body', { vendor, host, model })}</DialogDescription>
        </DialogHeader>
        <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">
          <li>{t('kb.embedding.disclosure.redacted')}</li>
          <li>{t('kb.embedding.disclosure.excluded')}</li>
          <li>{t('kb.embedding.disclosure.queries')}</li>
        </ul>
        {customEndpoint && (
          <div role="alert" className="flex items-start gap-2 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
            <AlertTriangle aria-hidden className="mt-0.5 size-4 shrink-0" />
            <p>{t('kb.embedding.disclosure.customEndpoint', { vendor, host })}</p>
          </div>
        )}
        {!redactionReady && (
          <div role="alert" className="flex items-start gap-2 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
            <AlertTriangle aria-hidden className="mt-0.5 size-4 shrink-0" />
            <p>
              {t('kb.embedding.disclosure.redactionOff')}
              {redactionReason ? ` (${redactionReason})` : ''}
            </p>
          </div>
        )}
        <label className="flex cursor-pointer items-start gap-2 text-sm text-foreground">
          <Checkbox checked={ack} onCheckedChange={(v) => setAck(v === true)} aria-label={t('kb.embedding.disclosure.ack', { vendor, host })} />
          <span>{t('kb.embedding.disclosure.ack', { vendor, host })}</span>
        </label>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button disabled={!ack || busy} onClick={onConfirm}>
            {t('kb.embedding.disclosure.confirm', { vendor })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
