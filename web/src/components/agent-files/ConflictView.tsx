import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { Button } from '@/components/mds';
import { toast } from '@/lib/toast';

export interface ConflictViewProps {
  /** What this person was about to save. */
  mine: string;
  /** What the file holds now (someone else saved in between). */
  current: string;
  onReload: () => void;
}

function Column({ title, text, testId }: { title: string; text: string; testId: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <p className="text-xs font-medium text-muted-foreground">{title}</p>
      <pre
        className="max-h-96 min-h-24 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-surface-border bg-muted p-2 font-mono text-xs"
        data-testid={testId}
      >
        {text}
      </pre>
    </div>
  );
}

/**
 * ConflictView — someone else saved the file while this person was editing.
 * Shows both texts side by side; "copy my content" keeps the edit on the
 * clipboard before "reload" brings in the latest file.
 */
export function ConflictView({ mine, current, onReload }: ConflictViewProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const copyMine = async () => {
    try {
      await navigator.clipboard.writeText(mine);
      toast.success(t('agentFiles.conflict.copied'));
    } catch {
      toast.error(t('agentFiles.conflict.copyFailed'));
    }
  };

  return (
    <section aria-label={t('agentFiles.review.title.conflict')} className="space-y-3" data-testid="conflict-view">
      <p role="alert" className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-sm text-foreground">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
        {t('agentFiles.conflict.body')}
      </p>
      <div className="grid gap-3 md:grid-cols-2">
        <Column title={t('agentFiles.conflict.mine')} text={mine} testId="conflict-mine" />
        <Column title={t('agentFiles.conflict.current')} text={current} testId="conflict-current" />
      </div>
      <div className="flex flex-wrap justify-end gap-2">
        <Button variant="outline" onClick={() => void copyMine()}>
          {t('agentFiles.conflict.copyMine')}
        </Button>
        <Button variant="brand" onClick={onReload}>
          {t('agentFiles.review.reload')}
        </Button>
      </div>
    </section>
  );
}
