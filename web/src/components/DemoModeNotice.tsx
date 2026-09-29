import { useState } from 'react';
import { useIntl } from 'react-intl';
import { FlaskConical, X } from 'lucide-react';
import { Button } from '@/components/mds';

/**
 * Demonstration-mode banner for the three exploratory lines (Decision Lab,
 * Causal evidence review, Reversible Context Store).
 *
 * The 2026-09-29 feature audit kept all three in the product but recorded that
 * none of them has yet run against real operator data — their own specs say so.
 * A reader arriving at one of these pages sees numbers, charts and verdicts and
 * has no way to know that. This one line says it, in the reader's language,
 * right under the page title.
 *
 * Dismissible per page, remembered in `localStorage` so it does not nag. The
 * storage read/write is wrapped: a browser with site data blocked still renders
 * the banner correctly (it simply comes back every visit, which is the safe
 * direction to fail).
 */

const STORAGE_PREFIX = 'duduclaw.demoNotice.dismissed.';

/** Where the data currently on screen came from. */
export type DemoDataOrigin = 'synthetic' | 'operator' | 'taskBoard' | 'auditTrail';

function readDismissed(pageKey: string): boolean {
  try {
    return localStorage.getItem(STORAGE_PREFIX + pageKey) === '1';
  } catch {
    return false;
  }
}

function writeDismissed(pageKey: string): void {
  try {
    localStorage.setItem(STORAGE_PREFIX + pageKey, '1');
  } catch {
    // Site data blocked — the banner simply reappears next visit.
  }
}

export interface DemoModeNoticeProps {
  /** Stable id for the dismissal memory, e.g. `decision-lab`. */
  pageKey: string;
  /**
   * `synthetic` (the default, and what every one of these pages shows before
   * anything is loaded) ⇒ the numbers come from generated fixtures.
   * `operator` ⇒ the operator uploaded their own pilot export; still
   * exploratory, but no longer synthetic, so the wording changes.
   * `taskBoard` / `auditTrail` (X1 2026-09-29) ⇒ the rows came from this
   * machine's own `tasks.db` / `tool_calls.jsonl`. Both messages name the
   * proxy or the co-occurrence limit in the same sentence, because "real"
   * here means "not generated", never "validated".
   *
   * Callers derive this from the backend's own status string
   * (`synthetic_only_exploratory` vs `operator_supplied_exploratory` /
   * `exploratory_operator_upload`) or from the row-verified queue-id prefix,
   * rather than guessing.
   */
  origin?: DemoDataOrigin;
}

export function DemoModeNotice({ pageKey, origin = 'synthetic' }: DemoModeNoticeProps) {
  const intl = useIntl();
  const [dismissed, setDismissed] = useState(() => readDismissed(pageKey));
  if (dismissed) return null;
  const messageId = {
    operator: 'demoMode.operator',
    taskBoard: 'demoMode.taskBoard',
    auditTrail: 'demoMode.auditTrail',
    synthetic: 'demoMode.synthetic',
  }[origin];
  return (
    <div
      role="status"
      data-testid="demo-mode-notice"
      className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/5 px-3 py-2 text-sm"
    >
      <FlaskConical className="mt-0.5 size-4 shrink-0 text-amber-700" aria-hidden="true" />
      <p className="flex-1 text-muted-foreground">{intl.formatMessage({ id: messageId })}</p>
      <Button
        variant="ghost"
        size="sm"
        aria-label={intl.formatMessage({ id: 'demoMode.dismiss' })}
        onClick={() => {
          writeDismissed(pageKey);
          setDismissed(true);
        }}
      >
        <X className="size-4" aria-hidden="true" />
      </Button>
    </div>
  );
}
