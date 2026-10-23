import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type TeamAgent } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  Textarea,
} from '@/components/mds';

/**
 * 交辦 from the 我的團隊 page: one goal for one report's AI employee, filed
 * with `tasks.create` (`kind: "goal"`). The gateway decides whether the
 * caller may hand this employee work; the button is only offered when the
 * team listing says so.
 *
 * After a successful assignment the viewer stays on 我的團隊 with a
 * confirmation: a manager who reaches the employee only through a report
 * cannot open the task itself yet, so jumping to it would land on a
 * permission error.
 */
export function TeamAssignDialog({
  agent,
  reportName,
  onClose,
}: {
  agent: TeamAgent;
  /** The report whose employee this is (shown in the description). */
  reportName: string;
  onClose: () => void;
}) {
  const intl = useIntl();
  const [goal, setGoal] = useState('');
  const [criteria, setCriteria] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const goalText = goal.trim();

  const handleSubmit = async () => {
    if (!goalText || submitting) return;
    setSubmitting(true);
    try {
      const { task } = await api.tasks.createGoal({
        agent: agent.name,
        description: goalText,
        acceptance_criteria: criteria.trim() || undefined,
      });
      toast.success(
        intl.formatMessage(
          { id: 'team.assign.success' },
          { name: agent.display_name, title: task.title || goalText },
        ),
      );
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {intl.formatMessage({ id: 'team.assign.title' }, { name: agent.display_name })}
          </DialogTitle>
          <DialogDescription>
            {intl.formatMessage({ id: 'team.assign.subtitle' }, { report: reportName })}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="team-assign-goal" className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'team.assign.goal' })}
            </label>
            <Textarea
              id="team-assign-goal"
              value={goal}
              rows={3}
              placeholder={intl.formatMessage({ id: 'team.assign.goalPlaceholder' })}
              onChange={(e) => setGoal(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <label htmlFor="team-assign-criteria" className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'team.assign.criteria' })}
            </label>
            <Textarea
              id="team-assign-criteria"
              value={criteria}
              rows={2}
              placeholder={intl.formatMessage({ id: 'team.assign.criteriaPlaceholder' })}
              onChange={(e) => setCriteria(e.target.value)}
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="brand" onClick={handleSubmit} disabled={!goalText || submitting}>
            {intl.formatMessage({ id: 'team.action.assign' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
