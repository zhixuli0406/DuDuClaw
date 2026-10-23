import { useIntl } from 'react-intl';
import { Archive } from 'lucide-react';
import type { SkillTarget } from '@/lib/api-skills-manage';
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
} from '@/components/mds';

/** Confirm removing employees' own skills (they are archived and can be restored). */
export function RemoveSkillsDialog({
  targets,
  nameOf,
  busy,
  onConfirm,
  onClose,
}: {
  targets: readonly SkillTarget[];
  nameOf: (agentId: string) => string;
  busy: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  const intl = useIntl();
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'skills.manage.removeTitle' }, { count: targets.length })}</DialogTitle>
          <DialogDescription>{intl.formatMessage({ id: 'skills.manage.removeDesc' })}</DialogDescription>
        </DialogHeader>
        <ul className="max-h-48 space-y-1 overflow-y-auto text-sm">
          {targets.map((t) => (
            <li key={`${t.agent_id}:${t.skill_name}`} className="truncate">
              <span className="font-medium text-foreground">{t.skill_name}</span>
              <span className="text-muted-foreground"> · {nameOf(t.agent_id)}</span>
            </li>
          ))}
        </ul>
        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="destructive" onClick={onConfirm} disabled={busy}>
            <Archive />
            {intl.formatMessage({ id: 'skills.manage.remove' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
