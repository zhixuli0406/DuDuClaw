import { useIntl } from 'react-intl';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';

export interface UnsavedChangesDialogProps {
  open: boolean;
  /** Keep editing. */
  onStay: () => void;
  /** Throw the edit away and go on. */
  onDiscard: () => void;
}

/** "You have unsaved changes" — asked before an edit would be lost. */
export function UnsavedChangesDialog({ open, onStay, onDiscard }: UnsavedChangesDialogProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onStay()}>
      <DialogContent className="sm:max-w-md" data-testid="unsaved-changes-dialog">
        <DialogHeader>
          <DialogTitle>{t('agentFiles.unsaved.title')}</DialogTitle>
          <DialogDescription>{t('agentFiles.unsaved.body')}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={onStay}>
            {t('agentFiles.unsaved.stay')}
          </Button>
          <Button variant="destructive" onClick={onDiscard}>
            {t('agentFiles.unsaved.discard')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
