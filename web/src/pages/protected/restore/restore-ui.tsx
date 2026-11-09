/**
 * Small shared pieces of the restore / adoption screens (v1.81 F5/F6e-UI):
 * status badges, the error notice, bidi-safe text and fixed item lists.
 */
import { Fragment, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { Badge } from '@/components/mds';
import { cn } from '@/lib/utils';
import { describeRestoreError } from '@/lib/restore-errors';
import { normalizeRootState, type RootState } from '@/lib/isolation-restore-api';
import { isStepUpCancelled } from './with-token';

export function useRestoreT() {
  const intl = useIntl();
  return (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
}

// ── Error notice ────────────────────────────────────────────────────────────

/** Friendly sentence for a known code, the server's own words beside it. */
export function RestoreErrorNotice({ error, className }: { error: unknown; className?: string }) {
  const t = useRestoreT();
  // Closing the step-up dialog is a choice, not a failure.
  if (error == null || isStepUpCancelled(error)) return null;
  const d = describeRestoreError(error);
  return (
    <div
      role="alert"
      className={cn('flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-foreground', className)}
    >
      <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden="true" />
      <div className="min-w-0 space-y-0.5 break-words">
        {d.friendlyId && <p className="font-medium">{t(d.friendlyId)}</p>}
        {d.detail && <p>{d.detail}</p>}
        {d.fallback && <p>{d.fallback}</p>}
      </div>
    </div>
  );
}

// ── Badges ──────────────────────────────────────────────────────────────────

export function RootStateBadge({ state }: { state: RootState | string | undefined }) {
  const t = useRestoreT();
  const s = normalizeRootState(state);
  const variant = s === 'restorable' ? 'secondary' : s === 'root_gone' ? 'destructive' : 'outline';
  return <Badge variant={variant}>{t(`restoreFlow.root.${s}`)}</Badge>;
}

const SEALS = ['sealed', 'oversize', 'invalid', 'absent', 'pending'] as const;

export function SealBadge({ seal }: { seal: string | undefined }) {
  const t = useRestoreT();
  const s = (SEALS as readonly string[]).includes(seal ?? '') ? (seal as string) : 'unknown';
  const variant = s === 'sealed' ? 'secondary' : s === 'invalid' || s === 'oversize' ? 'destructive' : 'outline';
  return <Badge variant={variant}>{t(`restoreFlow.seal.${s}`)}</Badge>;
}

export function KindBadge({ kind }: { kind: string | undefined }) {
  const t = useRestoreT();
  return <Badge variant="outline">{t(kind === 'shared' ? 'restoreFlow.kind.shared' : 'restoreFlow.kind.personal')}</Badge>;
}

export function DaysLeft({ days }: { days: number | null }) {
  const t = useRestoreT();
  if (days == null) return <span className="text-muted-foreground">{t('restoreFlow.days.unknown')}</span>;
  return (
    <span className={cn('font-mono text-xs tabular-nums', days <= 7 ? 'text-amber-700 dark:text-amber-400' : 'text-muted-foreground')}>
      {t('restoreFlow.days.left', { days })}
    </span>
  );
}

// ── Bidi-safe full text ─────────────────────────────────────────────────────

/** Characters that silently reorder displayed text (Trojan-Source class). */
const BIDI_CONTROL = /[؜‎‏‪-‮⁦-⁩]/;
const BIDI_CONTROL_G = /([؜‎‏‪-‮⁦-⁩])/g;

export function hasBidiControls(text: string): boolean {
  return BIDI_CONTROL.test(text);
}

/**
 * The whole text, never shortened. Line breaks kept (`pre-wrap`), each
 * paragraph laid out on its own direction (`unicode-bidi: plaintext`), and
 * every direction-control character replaced by a visible marker so it can
 * neither hide nor reorder anything.
 */
export function BidiSafeText({ text, id, className, label }: { text: string; id?: string; className?: string; label?: string }) {
  const parts = text.split(BIDI_CONTROL_G);
  return (
    <div
      id={id}
      dir="auto"
      role="region"
      aria-label={label}
      tabIndex={0}
      className={cn(
        'rounded-lg border border-surface-border bg-muted/40 p-3 text-sm whitespace-pre-wrap break-words [overflow-wrap:anywhere] [unicode-bidi:plaintext]',
        'focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50',
        className,
      )}
    >
      {parts.map((part, index) =>
        BIDI_CONTROL.test(part) && part.length === 1 ? (
          <span key={index} className="mx-px rounded bg-warning/15 px-0.5 font-mono text-xs" dir="ltr">
            {`[U+${part.codePointAt(0)!.toString(16).toUpperCase().padStart(4, '0')}]`}
          </span>
        ) : (
          <Fragment key={index}>{part}</Fragment>
        ),
      )}
    </div>
  );
}

// ── Item lists ──────────────────────────────────────────────────────────────

/** Known item keys the gateway may name in `carried` / `sealed` / `lost`. */
const ITEM_KEYS = ['settings', 'memory', 'wiki', 'files', 'sessions', 'work', 'connections', 'credentials', 'org', 'routes', 'runtime'] as const;

export function itemLabelId(key: string): string {
  return (ITEM_KEYS as readonly string[]).includes(key) ? `restoreFlow.item.${key}` : 'restoreFlow.item.other';
}

/** Dedupe by label, so unknown keys collapse into one "other" line. */
export function ItemList({ keys, className }: { keys: string[]; className?: string }) {
  const t = useRestoreT();
  const labels = Array.from(new Set(keys.map((key) => t(itemLabelId(key)))));
  return (
    <ul className={cn('list-disc space-y-1 ps-5 text-sm', className)}>
      {labels.map((label) => <li key={label}>{label}</li>)}
    </ul>
  );
}

export function TwoLists({
  leftTitle, left, rightTitle, right,
}: { leftTitle: ReactNode; left: ReactNode; rightTitle: ReactNode; right: ReactNode }) {
  return (
    <div className="grid gap-3 sm:grid-cols-2">
      <section className="rounded-lg border border-surface-border p-3">
        <h4 className="mb-2 text-sm font-medium">{leftTitle}</h4>
        {left}
      </section>
      <section className="rounded-lg border border-surface-border p-3">
        <h4 className="mb-2 text-sm font-medium">{rightTitle}</h4>
        {right}
      </section>
    </div>
  );
}

/** Numbered progress steps; `current` is 0-based, `done` marks all complete. */
export function ProgressSteps({ steps, current, label }: { steps: string[]; current: number; label: string }) {
  return (
    <ol aria-label={label} className="flex flex-wrap gap-x-4 gap-y-2 text-sm">
      {steps.map((step, index) => {
        const state = index < current ? 'done' : index === current ? 'current' : 'todo';
        return (
          <li key={step} aria-current={state === 'current' ? 'step' : undefined} className="flex items-center gap-2">
            <span
              className={cn(
                'inline-flex size-6 items-center justify-center rounded-full border font-mono text-xs tabular-nums',
                state === 'done' && 'border-primary bg-primary text-primary-foreground',
                state === 'current' && 'border-primary text-foreground',
                state === 'todo' && 'border-border text-muted-foreground',
              )}
              aria-hidden="true"
            >
              {index + 1}
            </span>
            <span className={state === 'todo' ? 'text-muted-foreground' : 'text-foreground'}>{step}</span>
          </li>
        );
      })}
    </ol>
  );
}

/** Restored / carried items — the same static plain-language lists every
 *  screen uses, so the owner and the Admin read identical words. */
export const RESTORED_ITEMS = ['settings', 'memory', 'wiki', 'files', 'sessions'];
export const ADMIN_RESETUP_ITEMS = ['credentials', 'org', 'routes'];
export const ADOPTION_CARRIED = ['settings', 'memory', 'wiki', 'files'];
export const ADOPTION_NOT_CARRIED = ['sessions', 'connections', 'credentials', 'runtime'];
export const EARLY_PURGE_LOST = ['settings', 'memory', 'wiki', 'files', 'sessions'];
