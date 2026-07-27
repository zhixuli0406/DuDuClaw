import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Check, Plus, Search } from 'lucide-react';
import { api, type BuiltinToolEntry } from '@/lib/api';
import { cn } from '@/lib/utils';
import { Popover, PopoverTrigger, PopoverContent, Badge } from '@/components/mds';
import { inputClass } from './controlClass';

// ── Shared, TTL-less module cache ────────────────────────────────────────────
// The catalog is static per binary build, so one fetch per session is enough.
// Both the allowed- and denied-tools pickers share this so opening them does
// not double-fetch. On error we surface it — never fabricate a list.
let cache: BuiltinToolEntry[] | null = null;
let inflight: Promise<BuiltinToolEntry[]> | null = null;

export function fetchCatalog(): Promise<BuiltinToolEntry[]> {
  if (cache) return Promise.resolve(cache);
  if (inflight) return inflight;
  inflight = api.tools
    .builtinCatalog()
    .then((res) => {
      cache = res?.tools ?? [];
      return cache;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

interface ToolCatalogPickerProps {
  /** Trigger button label (i18n string). */
  triggerLabel: string;
  /** Current selected values (the `qualified` strings already in the list). */
  selected: ReadonlyArray<string>;
  /** Called with a fresh array whenever a tool is added/removed. */
  onChange: (next: string[]) => void;
}

/**
 * A searchable, category-grouped picker of built-in tools (DuDuClaw MCP tools +
 * native Claude tools). Clicking a tool toggles its `qualified` string in the
 * bound list. Complements the free-text `ChipEditor` (custom MCP tool names /
 * wildcards still go there). New tools appear automatically because the catalog
 * comes from the single source of truth in `duduclaw-core`.
 */
export function ToolCatalogPicker({ triggerLabel, selected, onChange }: ToolCatalogPickerProps) {
  const intl = useIntl();
  const t = (id: string, def?: string) => intl.formatMessage({ id, defaultMessage: def ?? id });
  const [open, setOpen] = useState(false);
  const [tools, setTools] = useState<BuiltinToolEntry[]>(cache ?? []);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');

  useEffect(() => {
    if (!open || cache) return;
    let alive = true;
    setLoading(true);
    setError(null);
    fetchCatalog()
      .then((list) => {
        if (alive) setTools(list);
      })
      .catch((e) => {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [open]);

  const selectedSet = useMemo(() => new Set(selected), [selected]);

  // Filter by name / qualified / description, then group by category preserving
  // the catalog's original ordering.
  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = q
      ? tools.filter(
          (e) =>
            e.name.toLowerCase().includes(q) ||
            e.qualified.toLowerCase().includes(q) ||
            e.description.toLowerCase().includes(q),
        )
      : tools;
    const byCat = new Map<string, BuiltinToolEntry[]>();
    for (const e of filtered) {
      const arr = byCat.get(e.category) ?? [];
      arr.push(e);
      byCat.set(e.category, arr);
    }
    return Array.from(byCat.entries());
  }, [tools, query]);

  const toggle = (qualified: string) => {
    if (selectedSet.has(qualified)) {
      onChange(selected.filter((v) => v !== qualified));
    } else {
      onChange([...selected, qualified]);
    }
  };

  const catLabel = (cat: string) =>
    intl.formatMessage({ id: `agents.cap.toolCat.${cat}`, defaultMessage: cat });

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg border border-border bg-background px-2.5 text-xs font-medium text-foreground outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50">
        <Plus className="h-3.5 w-3.5" />
        {triggerLabel}
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 p-0">
        <div className="border-b border-border/60 p-2">
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t('agents.cap.toolPicker.search', 'Search tools…')}
              className={cn(inputClass, 'pl-8')}
              autoFocus
            />
          </div>
        </div>
        <div className="max-h-80 overflow-y-auto p-1">
          {loading && (
            <p className="px-2 py-6 text-center text-xs text-muted-foreground">
              {t('common.loading', 'Loading…')}
            </p>
          )}
          {error && (
            <p className="px-2 py-6 text-center text-xs text-rose-500">
              {t('agents.cap.toolPicker.error', 'Failed to load tool catalog')}: {error}
            </p>
          )}
          {!loading && !error && groups.length === 0 && (
            <p className="px-2 py-6 text-center text-xs text-muted-foreground">
              {t('agents.cap.toolPicker.empty', 'No matching tools')}
            </p>
          )}
          {!loading &&
            !error &&
            groups.map(([cat, entries]) => (
              <div key={cat} className="mb-1">
                <div className="px-2 pb-0.5 pt-1.5 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
                  {catLabel(cat)}
                </div>
                {entries.map((e) => {
                  const isSel = selectedSet.has(e.qualified);
                  return (
                    <button
                      key={e.qualified}
                      type="button"
                      onClick={() => toggle(e.qualified)}
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
                          <span className="truncate font-mono text-xs font-medium">{e.name}</span>
                          {e.scope && (
                            <Badge variant="outline" className="shrink-0 px-1 py-0 text-[9px]">
                              {e.scope}
                            </Badge>
                          )}
                        </span>
                        <span className="mt-0.5 block truncate text-[11px] text-muted-foreground">
                          {e.description}
                        </span>
                      </span>
                    </button>
                  );
                })}
              </div>
            ))}
        </div>
      </PopoverContent>
    </Popover>
  );
}
