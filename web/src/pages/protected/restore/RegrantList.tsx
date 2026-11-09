import { useState } from 'react';
import { CheckCircle2 } from 'lucide-react';
import { Badge, Button } from '@/components/mds';
import { adminRestoreApi, regrantId, type RegrantEntry } from '@/lib/isolation-restore-api';
import { RestoreErrorNotice, useRestoreT } from './restore-ui';
import { isStepUpCancelled, type WithToken } from './with-token';

const KINDS = ['website_credential', 'org_edge', 'route'] as const;

/** `website_credential:<id>` style ids name their kind; objects may say it. */
export function regrantKind(entry: RegrantEntry): string {
  if (typeof entry !== 'string' && entry.kind) return entry.kind;
  const id = regrantId(entry);
  const prefix = id.split(':', 1)[0];
  return (KINDS as readonly string[]).includes(prefix) ? prefix : 'other';
}

function regrantName(entry: RegrantEntry): string {
  if (typeof entry !== 'string' && entry.label) return entry.label;
  const id = regrantId(entry);
  const i = id.indexOf(':');
  return i > 0 && (KINDS as readonly string[]).includes(id.slice(0, i)) ? id.slice(i + 1) : id;
}

/**
 * After a restore: what the Admin must grant again (website sign-ins, links
 * to other employees, account routes). One click each, with step-up.
 * `skipped` / `listed` entries are shown for the record only.
 */
export function RegrantList({
  employeeId,
  incarnation,
  items,
  skipped = [],
  listed = [],
  withToken,
}: {
  employeeId: string;
  incarnation: number;
  items: RegrantEntry[];
  skipped?: RegrantEntry[];
  listed?: RegrantEntry[];
  withToken: WithToken;
}) {
  const t = useRestoreT();
  const [done, setDone] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(items.filter((i) => typeof i !== 'string' && i.done).map((i) => [regrantId(i), true])),
  );
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<{ id: string; error: unknown } | null>(null);

  const grant = async (entry: RegrantEntry) => {
    const id = regrantId(entry);
    setBusy(id);
    setError(null);
    try {
      await withToken((token) => adminRestoreApi.regrant(employeeId, incarnation, id, token));
      setDone((prev) => ({ ...prev, [id]: true }));
    } catch (e) {
      if (!isStepUpCancelled(e)) setError({ id, error: e });
    } finally {
      setBusy(null);
    }
  };

  if (items.length === 0 && skipped.length === 0 && listed.length === 0) {
    return <p className="text-sm text-muted-foreground">{t('restoreFlow.regrant.none')}</p>;
  }

  const row = (entry: RegrantEntry, action: boolean) => {
    const id = regrantId(entry);
    const name = regrantName(entry);
    const kindLabel = t(`restoreFlow.regrant.kind.${regrantKind(entry)}`);
    return (
      <li key={id} className="space-y-1.5 px-3 py-2">
        <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
          <span className="min-w-0">
            <span className="block text-xs text-muted-foreground">{kindLabel}</span>
            <span className="block truncate" title={name}>{name}</span>
          </span>
          {action ? (
            done[id] ? (
              <span role="status" className="flex items-center gap-1 text-sm text-success">
                <CheckCircle2 className="size-4" aria-hidden="true" /> {t('restoreFlow.regrant.done')}
              </span>
            ) : (
              <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => void grant(entry)}
                aria-label={t('restoreFlow.action.forRow', { action: t('restoreFlow.regrant.grant'), name })}>
                {busy === id ? t('restoreFlow.common.sending') : t('restoreFlow.regrant.grant')}
              </Button>
            )
          ) : (
            <Badge variant="outline">{t(skipped.includes(entry) ? 'restoreFlow.regrant.skipped' : 'restoreFlow.regrant.listed')}</Badge>
          )}
        </div>
        {error?.id === id && <RestoreErrorNotice error={error.error} />}
      </li>
    );
  };

  return (
    <div className="space-y-2">
      <p className="text-sm text-muted-foreground">{t('restoreFlow.regrant.desc')}</p>
      <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
        {items.map((entry) => row(entry, true))}
        {skipped.map((entry) => row(entry, false))}
        {listed.map((entry) => row(entry, false))}
      </ul>
    </div>
  );
}
