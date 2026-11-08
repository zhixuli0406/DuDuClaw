import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { ChevronDown, X } from 'lucide-react';
import { api, type MemoryPrivacySettings, type MemoryPrivacyUpdate } from '@/lib/api';
import { isNewFeature } from '@/apps/registry';
import { isImeComposing } from '@/lib/keyboard';
import { formatError, toast } from '@/lib/toast';
import { useSystemStore } from '@/stores/system-store';
import { Button, Input, SettingsCard, SettingsSaveState, Switch } from '@/components/mds';

/** RM-039 built-in categories, in display order (all excluded by default). */
export const MEMORY_PRIVACY_BUILTINS = ['health', 'family', 'personal_finance', 'salary'] as const;

/** What the gateway enforces when `[memory_privacy]` is absent (or unreadable). */
export const MEMORY_PRIVACY_DEFAULTS: MemoryPrivacySettings = {
  excluded_categories: [...MEMORY_PRIVACY_BUILTINS],
  custom_keywords: {},
  builtin_categories: [...MEMORY_PRIVACY_BUILTINS],
  load_state: 'defaults',
};

const NEW_IN = '1.81.0';
const MAX_KEYWORD_LENGTH = 40;

/** Stable order (built-ins first, then custom categories as given, no
 *  duplicates) so toggling a switch off and on again is not a "change". */
export function canonicalPrivacy(list: readonly string[]): string[] {
  const builtins: readonly string[] = MEMORY_PRIVACY_BUILTINS;
  const set = new Set(list);
  const custom = list.filter((c, i) => !builtins.includes(c) && list.indexOf(c) === i);
  return [...builtins.filter((c) => set.has(c)), ...custom];
}

/**
 * 「自動記憶不記錄的內容」 — four built-in category switches plus per-category
 * custom keyword chips. Every change saves immediately (one `memory_privacy`
 * key per request, replace semantics) and rolls back with a message if the
 * gateway refuses it. Categories this card does not show (custom ones set in
 * config.toml) are carried through untouched.
 */
export function MemoryPrivacyCard({
  settings,
  onSettingsChange,
  disabled = false,
}: {
  settings: MemoryPrivacySettings;
  onSettingsChange: (next: MemoryPrivacySettings) => void;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: `settings.system.memoryPrivacy.${id}` }, values);
  const version = useSystemStore((s) => s.status?.version ?? null);

  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const savedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimer.current) clearTimeout(savedTimer.current); }, []);

  const save = async (update: MemoryPrivacyUpdate, next: MemoryPrivacySettings): Promise<boolean> => {
    if (busy) return false;
    setBusy(true);
    setSaved(false);
    const previous = settings;
    onSettingsChange(next); // optimistic
    try {
      await api.system.updateMemoryPrivacy(update);
      setSaved(true);
      if (savedTimer.current) clearTimeout(savedTimer.current);
      savedTimer.current = setTimeout(() => setSaved(false), 2000);
      return true;
    } catch (e) {
      onSettingsChange(previous);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const toggle = (cat: string, on: boolean) => {
    const excluded = canonicalPrivacy(
      on ? [...settings.excluded_categories, cat] : settings.excluded_categories.filter((c) => c !== cat),
    );
    void save({ excluded_categories: excluded }, { ...settings, excluded_categories: excluded });
  };

  const writeKeywords = (cat: string, list: string[]) => {
    const custom_keywords = { ...settings.custom_keywords };
    if (list.length > 0) custom_keywords[cat] = list;
    else delete custom_keywords[cat];
    return save({ custom_keywords }, { ...settings, custom_keywords });
  };

  const addKeyword = async (cat: string) => {
    const word = (drafts[cat] ?? '').trim().replace(/\s+/g, ' ');
    if (word === '') return;
    const current = settings.custom_keywords[cat] ?? [];
    if (current.some((k) => k.toLowerCase() === word.toLowerCase())) {
      setDrafts((d) => ({ ...d, [cat]: '' }));
      return;
    }
    if (await writeKeywords(cat, [...current, word])) setDrafts((d) => ({ ...d, [cat]: '' }));
  };

  const removeKeyword = (cat: string, word: string) =>
    writeKeywords(cat, (settings.custom_keywords[cat] ?? []).filter((k) => k !== word));

  const frozen = disabled || busy;

  return (
    <SettingsCard aria-labelledby="memory-privacy-title">
      <div className="space-y-1.5 px-4 pb-3.5 pt-4">
        <div className="flex flex-wrap items-center gap-2">
          <h4 id="memory-privacy-title" className="text-[15px] font-medium">{t('title')}</h4>
          {isNewFeature(NEW_IN, version) && (
            <span className="inline-flex h-5 items-center rounded-full bg-brand/15 px-2 text-[11px] font-medium text-brand">
              {t('new')}
            </span>
          )}
        </div>
        <p className="text-xs text-muted-foreground">{t('help')}</p>
        {settings.load_state === 'failed_closed' && (
          <p role="alert" className="text-xs text-warning">{t('failedClosed')}</p>
        )}
      </div>

      {MEMORY_PRIVACY_BUILTINS.map((cat) => {
        const label = t(`category.${cat}`);
        const keywords = settings.custom_keywords[cat] ?? [];
        const expanded = open === cat;
        const panelId = `memory-privacy-kw-${cat}`;
        return (
          <div key={cat} className="space-y-3 px-4 py-3.5" data-category={cat}>
            <div className="flex items-center justify-between gap-4">
              <div className="min-w-0 space-y-0.5">
                <div className="text-sm font-medium">{label}</div>
                <div className="text-xs text-muted-foreground">{t(`category.${cat}.desc`)}</div>
                <button
                  type="button"
                  aria-expanded={expanded}
                  aria-controls={panelId}
                  onClick={() => setOpen(expanded ? null : cat)}
                  className="inline-flex min-h-6 items-center gap-1 rounded text-xs text-brand underline-offset-2 hover:underline focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
                >
                  {keywords.length === 0 ? t('keywords.none') : t('keywords.count', { count: keywords.length })}
                  <ChevronDown aria-hidden="true" className={expanded ? 'size-3 rotate-180' : 'size-3'} />
                </button>
              </div>
              <Switch
                checked={settings.excluded_categories.includes(cat)}
                onCheckedChange={(v) => toggle(cat, Boolean(v))}
                disabled={frozen}
                aria-label={t('switch.aria', { category: label })}
              />
            </div>

            {expanded && (
              <div id={panelId} className="space-y-2.5 rounded-lg bg-muted/50 px-3.5 py-3">
                <span className="text-xs font-medium text-muted-foreground">{t('keywords.title')}</span>
                {keywords.length > 0 && (
                  <ul className="flex flex-wrap gap-1.5" aria-label={t('keywords.title')}>
                    {keywords.map((word) => (
                      <li
                        key={word}
                        className="inline-flex h-[26px] items-center gap-1 rounded-full bg-secondary py-0 pl-2.5 pr-1.5 text-[13px] text-secondary-foreground"
                      >
                        <span className="max-w-[14rem] truncate">{word}</span>
                        <button
                          type="button"
                          disabled={frozen}
                          onClick={() => void removeKeyword(cat, word)}
                          aria-label={t('keywords.remove', { keyword: word })}
                          className="inline-flex size-5 items-center justify-center rounded-full text-muted-foreground hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none disabled:opacity-50"
                        >
                          <X aria-hidden="true" className="size-3" />
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
                <div className="flex gap-2">
                  <Input
                    type="text"
                    value={drafts[cat] ?? ''}
                    maxLength={MAX_KEYWORD_LENGTH}
                    disabled={frozen}
                    onChange={(e) => setDrafts((d) => ({ ...d, [cat]: e.target.value }))}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && !isImeComposing(e)) {
                        e.preventDefault();
                        void addKeyword(cat);
                      }
                    }}
                    placeholder={t('keywords.placeholder')}
                    aria-label={t('keywords.input.aria', { category: label })}
                    className="flex-1"
                  />
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={frozen || (drafts[cat] ?? '').trim() === ''}
                    onClick={() => void addKeyword(cat)}
                    aria-label={t('keywords.add.aria', { category: label })}
                  >
                    {intl.formatMessage({ id: 'common.add' })}
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground">{t('keywords.help')}</p>
              </div>
            )}
          </div>
        );
      })}

      <div className="flex min-h-11 items-center justify-between gap-3 bg-muted/40 px-4 py-2.5 text-xs">
        <span className="text-muted-foreground">{t('defaultsNote')}</span>
        <SettingsSaveState
          status={busy ? 'saving' : saved ? 'saved' : 'idle'}
          savingLabel={intl.formatMessage({ id: 'common.saving' })}
          savedLabel={intl.formatMessage({ id: 'settings.system.memoryPrivacy.saved' })}
        />
      </div>
    </SettingsCard>
  );
}
