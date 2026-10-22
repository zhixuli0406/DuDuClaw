import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { ArrowRight, Library } from 'lucide-react';
import { Checkbox, SettingsCard, SettingsSaveState, SettingsSection, type SettingsSaveStatus } from '@/components/mds';
import { formatError } from '@/lib/toast';
import { knowledgeApi, type KbAgentCollections } from '@/lib/knowledge-api';
import { SensitivityBadge } from '@/pages/knowledge/labels';

export type BindingMode = 'all' | 'selected' | 'off';

/** The saved `[knowledge] collections` value as a mode + selection. */
export function bindingFromSaved(bound: string[] | null): { mode: BindingMode; selected: string[] } {
  if (bound === null) return { mode: 'all', selected: [] };
  if (bound.length === 0) return { mode: 'off', selected: [] };
  return { mode: 'selected', selected: bound };
}

/** What to write: `null` = every granted collection, `[]` = none. Only
 *  granted slugs are sent (the gateway refuses anything else). */
export function bindingToWrite(mode: BindingMode, selected: readonly string[], granted: readonly string[]): string[] | null {
  if (mode === 'all') return null;
  if (mode === 'off') return [];
  return selected.filter((s) => granted.includes(s));
}

/**
 * 文件知識庫 section of the employee edit page. Collections are shared with
 * an AI employee (or its department) on the collection's own 權限 tab; here
 * the owner picks which of those this employee actually uses. Saves on
 * every change, like the rest of the page.
 */
export function KnowledgeBindingSection({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const navigate = useNavigate();
  const [data, setData] = useState<KbAgentCollections | null>(null);
  const [hidden, setHidden] = useState(false);
  const [mode, setMode] = useState<BindingMode>('all');
  const [selected, setSelected] = useState<string[]>([]);
  const [status, setStatus] = useState<SettingsSaveStatus>('idle');
  const [saveError, setSaveError] = useState<string | null>(null);
  const seq = useRef(0);

  useEffect(() => {
    let alive = true;
    knowledgeApi
      .agentCollections(agentId)
      .then((d) => {
        if (!alive) return;
        setData(d);
        const b = bindingFromSaved(d.bound);
        setMode(b.mode);
        setSelected(b.selected);
      })
      // Knowledge base switched off, or no access: the section is not shown.
      .catch(() => alive && setHidden(true));
    return () => {
      alive = false;
    };
  }, [agentId]);

  const granted = (data?.granted ?? []).map((c) => c.slug);

  const persist = useCallback(
    async (nextMode: BindingMode, nextSelected: string[]) => {
      const n = ++seq.current;
      setStatus('saving');
      setSaveError(null);
      try {
        await knowledgeApi.bindAgent(agentId, bindingToWrite(nextMode, nextSelected, granted));
        if (n === seq.current) setStatus('saved');
      } catch (e) {
        if (n !== seq.current) return;
        setStatus('error');
        setSaveError(formatError(e));
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [agentId, granted.join('\u0000')],
  );

  if (hidden || !data) return null;

  const change = (nextMode: BindingMode, nextSelected = selected) => {
    setMode(nextMode);
    setSelected(nextSelected);
    void persist(nextMode, nextSelected);
  };
  const toggle = (slug: string) => {
    const next = selected.includes(slug) ? selected.filter((s) => s !== slug) : [...selected, slug];
    change('selected', next);
  };
  const stale = selected.filter((s) => !granted.includes(s));

  const modes: Array<{ value: BindingMode; label: string; hint: string }> = [
    { value: 'all', label: t('kb.bind.mode.all'), hint: t('kb.bind.mode.allHint') },
    { value: 'selected', label: t('kb.bind.mode.selected'), hint: t('kb.bind.mode.selectedHint') },
    { value: 'off', label: t('kb.bind.mode.off'), hint: t('kb.bind.mode.offHint') },
  ];

  return (
    <SettingsSection title={t('kb.bind.title')} description={t('kb.bind.desc')}>
      <SettingsCard>
        <fieldset className="space-y-2 px-4 py-3">
          <legend className="sr-only">{t('kb.bind.title')}</legend>
          {modes.map((m) => (
            <label key={m.value} className="flex cursor-pointer items-start gap-3">
              <input
                type="radio"
                name={`kb-bind-${agentId}`}
                value={m.value}
                checked={mode === m.value}
                onChange={() => change(m.value)}
                className="mt-1 accent-[var(--brand)]"
              />
              <span>
                <span className="block text-sm">{m.label}</span>
                <span className="block text-xs text-muted-foreground">{m.hint}</span>
              </span>
            </label>
          ))}
        </fieldset>
        {data.granted.length === 0 ? (
          <div className="px-4 py-4 text-center">
            <p className="text-xs text-muted-foreground">{t('kb.bind.noneGranted')}</p>
          </div>
        ) : (
          data.granted.map((c) => (
            <label
              key={c.id}
              className={
                'flex items-center gap-3 px-4 py-3 ' +
                (mode === 'selected' ? 'cursor-pointer hover:bg-surface-hover' : 'opacity-60')
              }
            >
              <Checkbox
                disabled={mode !== 'selected'}
                checked={mode === 'all' || (mode === 'selected' && selected.includes(c.slug))}
                onCheckedChange={() => toggle(c.slug)}
                aria-label={c.name}
              />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm text-foreground">{c.name}</span>
              </span>
              <SensitivityBadge value={c.sensitivity} />
            </label>
          ))
        )}
      </SettingsCard>
      {data.unreadable && <p className="px-1 text-xs text-destructive">{t('kb.bind.unreadable')}</p>}
      {mode === 'selected' && stale.length > 0 && (
        <p className="px-1 text-xs text-amber-700 dark:text-amber-400">{t('kb.bind.stale', { list: stale.join('、') })}</p>
      )}
      <div className="flex flex-wrap items-center justify-between gap-2 px-1">
        <button
          type="button"
          onClick={() => navigate('/knowledge-base')}
          className="inline-flex items-center gap-1 text-xs font-medium text-brand underline-offset-2 hover:underline"
        >
          <Library aria-hidden className="size-3.5" />
          {t('kb.bind.manage')}
          <ArrowRight aria-hidden className="size-3.5" />
        </button>
        <SettingsSaveState
          status={status}
          savingLabel={t('common.saving')}
          savedLabel={t('common.saved')}
          errorLabel={saveError ?? t('common.saveError')}
        />
      </div>
    </SettingsSection>
  );
}
