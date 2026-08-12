import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Check, ChevronDown, KeyRound, Key, X } from 'lucide-react';
import { api, type AccountInfo } from '@/lib/api';
import { cn } from '@/lib/utils';
import { Popover, PopoverTrigger, PopoverContent } from '@/components/mds';
import { ChipEditor } from './ChipEditor';

interface AccountPoolSelectProps {
  /** Currently-picked account ids (immutable — parent owns state). */
  values: ReadonlyArray<string>;
  /** Called with a brand-new array whenever the picks change. */
  onChange: (next: string[]) => void;
}

/**
 * AccountPoolSelect — picks account ids from the accounts the deployment
 * actually has, instead of asking the operator to retype them from memory.
 *
 * WP-C (§2-2 of the IA scatter audit): `[model] account_pool` used to be a
 * free-text `ChipEditor` whose placeholder was a made-up id (`oauth-pro`), so
 * the only way to fill it correctly was to open 帳號管理 in another tab, read an
 * id off a card and retype it. The list here comes from the SAME RPC the
 * accounts page renders (`accounts.budget_summary` → `accounts[]`), so the two
 * surfaces can never disagree.
 *
 * Fail-open is deliberate: if the accounts RPC errors (permission, gateway
 * restart) or the deployment has no accounts yet, this degrades to the original
 * free-text editor rather than trapping the operator in an empty dropdown.
 * Values already stored that no longer match a live account are never dropped —
 * they render as flagged chips so a renamed/removed account is visible instead
 * of silently disappearing on the next save.
 */
export function AccountPoolSelect({ values, onChange }: AccountPoolSelectProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [accounts, setAccounts] = useState<AccountInfo[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let alive = true;
    api.accounts
      .budgetSummary()
      .then((res) => {
        if (!alive) return;
        setAccounts(res?.accounts ?? []);
      })
      .catch(() => {
        if (!alive) return;
        setFailed(true);
        setAccounts([]);
      });
    return () => {
      alive = false;
    };
  }, []);

  const selectedSet = useMemo(() => new Set(values), [values]);
  const knownIds = useMemo(() => new Set((accounts ?? []).map((a) => a.id)), [accounts]);
  // Stored ids with no live account behind them — surfaced, never dropped.
  const orphans = useMemo(() => values.filter((v) => !knownIds.has(v)), [values, knownIds]);

  const toggle = (id: string) => {
    if (selectedSet.has(id)) {
      onChange(values.filter((v) => v !== id));
    } else {
      onChange([...values, id]);
    }
  };

  // Degraded path — no live list to pick from. Keep the free-text editor so the
  // field stays usable (and say why it looks different).
  if (accounts !== null && accounts.length === 0) {
    return (
      <div className="space-y-2">
        <ChipEditor
          values={values}
          onChange={onChange}
          placeholder={t('agentForm.brain.accountPool.manualPlaceholder')}
          addLabel={t('common.add')}
        />
        <p className="text-xs text-muted-foreground">
          {failed ? t('agentForm.brain.accountPool.loadFailed') : t('agentForm.brain.accountPool.noAccounts')}
        </p>
      </div>
    );
  }

  const typeLabel = (a: AccountInfo) =>
    (a.auth_method ?? a.account_type) === 'apikey'
      ? t('agentForm.brain.accountPool.typeApiKey')
      : t('agentForm.brain.accountPool.typeOauth');

  return (
    <div className="space-y-2">
      {values.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {values.map((v) => {
            const missing = !knownIds.has(v);
            return (
              <span
                key={v}
                className={cn(
                  'inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-xs font-medium',
                  missing ? 'bg-warning/15 text-warning' : 'bg-brand/12 text-brand',
                )}
                title={missing ? t('agentForm.brain.accountPool.missing') : undefined}
              >
                {v}
                <button
                  type="button"
                  onClick={() => onChange(values.filter((x) => x !== v))}
                  className="rounded-full p-0.5 hover:bg-foreground/10"
                  aria-label={`remove ${v}`}
                >
                  <X className="h-3 w-3" />
                </button>
              </span>
            );
          })}
        </div>
      )}

      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-border bg-background px-2.5 text-xs font-medium text-foreground outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50">
          {values.length > 0
            ? intl.formatMessage({ id: 'agentForm.brain.accountPool.selected' }, { count: values.length })
            : t('agentForm.brain.accountPool.pick')}
          <ChevronDown className="h-3.5 w-3.5" />
        </PopoverTrigger>
        <PopoverContent align="start" className="w-80 p-1">
          <div className="max-h-80 overflow-y-auto">
            {accounts === null ? (
              <p className="px-2 py-6 text-center text-xs text-muted-foreground">{t('common.loading')}</p>
            ) : (
              accounts.map((a) => {
                const isSel = selectedSet.has(a.id);
                return (
                  <button
                    key={a.id}
                    type="button"
                    onClick={() => toggle(a.id)}
                    className={cn(
                      'flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left hover:bg-muted/60',
                      isSel && 'bg-brand/8',
                    )}
                  >
                    <span
                      className={cn(
                        'mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded border',
                        isSel ? 'border-brand bg-brand text-white' : 'border-input',
                      )}
                    >
                      {isSel && <Check className="h-3 w-3" />}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-1.5">
                        <span className="truncate text-xs font-medium text-foreground">
                          {a.label?.trim() ? a.label : a.id}
                        </span>
                        <span className="inline-flex shrink-0 items-center gap-1 rounded-4xl bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                          {(a.auth_method ?? a.account_type) === 'apikey' ? (
                            <Key className="h-2.5 w-2.5" />
                          ) : (
                            <KeyRound className="h-2.5 w-2.5" />
                          )}
                          {typeLabel(a)}
                        </span>
                      </span>
                      <span className="mt-0.5 block truncate font-mono text-[10px] text-muted-foreground">
                        {a.id}
                      </span>
                    </span>
                  </button>
                );
              })
            )}
          </div>
        </PopoverContent>
      </Popover>

      {orphans.length > 0 && (
        <p className="text-xs text-warning">{t('agentForm.brain.accountPool.missing')}</p>
      )}
    </div>
  );
}
