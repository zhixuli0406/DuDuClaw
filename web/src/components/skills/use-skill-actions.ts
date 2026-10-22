import { useCallback, useState } from 'react';
import { useIntl } from 'react-intl';
import { toast, formatError } from '@/lib/toast';
import { failedTargets, skillsManageApi, type SkillTarget, type SkillTargetResult } from '@/lib/api-skills-manage';

/**
 * Switch / remove actions with one summary toast per request, then
 * `onDone()` so the caller refetches. `busy` is true while a request runs.
 */
export function useSkillActions(onDone: () => void) {
  const intl = useIntl();
  const [busy, setBusy] = useState(false);

  const report = useCallback(
    (results: readonly SkillTargetResult[], okId: string) => {
      const failed = failedTargets(results);
      const done = results.length - failed.length;
      if (done > 0) toast.success(intl.formatMessage({ id: okId }, { count: done }));
      if (failed.length > 0) {
        toast.error(
          intl.formatMessage(
            { id: 'skills.manage.partialFailure' },
            { count: failed.length, message: failed[0]?.error ?? '' },
          ),
        );
      }
    },
    [intl],
  );

  const run = useCallback(
    async (call: () => Promise<{ results: SkillTargetResult[] }>, okId: string) => {
      setBusy(true);
      try {
        const res = await call();
        report(res?.results ?? [], okId);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy(false);
        onDone();
      }
    },
    [intl, onDone, report],
  );

  const setEnabled = useCallback(
    (targets: SkillTarget[], enabled: boolean) =>
      targets.length === 0
        ? Promise.resolve()
        : run(
            () => skillsManageApi.setEnabled(targets, enabled),
            enabled ? 'skills.manage.enabledDone' : 'skills.manage.disabledDone',
          ),
    [run],
  );

  const remove = useCallback(
    (targets: SkillTarget[]) =>
      targets.length === 0
        ? Promise.resolve()
        : run(() => skillsManageApi.remove(targets), 'skills.manage.removedDone'),
    [run],
  );

  return { busy, setEnabled, remove };
}
