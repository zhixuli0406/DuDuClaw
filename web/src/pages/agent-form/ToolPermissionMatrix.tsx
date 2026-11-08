import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { ChevronRight, Lock, Search } from 'lucide-react';
import type { ActionRule, BuiltinToolEntry } from '@/lib/api';
import { cn } from '@/lib/utils';
import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';
import { fetchCatalog } from '@/components/shared/ToolCatalogPicker';
import { inputClass } from '@/components/shared/controlClass';
import {
  changedLists,
  computeNextLists,
  rowState,
  summarize,
  visibleTools,
  type Desired,
  type NextLists,
  type ToolLists,
} from './toolPermissionModel';

/** Groups expanded on first paint (the rest start collapsed; a search expands all). */
const INITIAL_OPEN_GROUPS = 3;

interface ToolPermissionMatrixProps {
  agentName: string;
  allowedTools: ReadonlyArray<string>;
  deniedTools: ReadonlyArray<string>;
  approvalRequiredTools: ReadonlyArray<string>;
  actionRules?: ReadonlyArray<ActionRule>;
  /** Only an admin may change anything; everyone else gets the read-only view. */
  isAdmin: boolean;
  /** Receives ONLY the lists that changed, as the `capabilities` payload keys. */
  onApply: (next: Partial<NextLists>) => void;
}

/**
 * 工具權限 — one row per catalog tool, two columns (可使用 / 需審批). All the
 * mapping rules (allow/deny/wildcard/approval) live in `toolPermissionModel.ts`.
 * Edits stay local (a draft) until the operator confirms the dialog; the
 * confirmed lists go out through the page's normal `capabilities` save.
 */
export function ToolPermissionMatrix({
  agentName,
  allowedTools,
  deniedTools,
  approvalRequiredTools,
  actionRules,
  isAdmin,
  onApply,
}: ToolPermissionMatrixProps) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [tools, setTools] = useState<BuiltinToolEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [query, setQuery] = useState('');
  const [closedGroups, setClosedGroups] = useState<Set<string>>(new Set());
  const [openedGroups, setOpenedGroups] = useState<Set<string>>(new Set());
  // Draft: only the fields the operator touched, keyed by qualified name.
  const [draft, setDraft] = useState<Map<string, Partial<Desired>>>(new Map());
  const [confirmOpen, setConfirmOpen] = useState(false);

  useEffect(() => {
    let alive = true;
    setError(null);
    fetchCatalog()
      .then((list) => alive && setTools(list))
      .catch((e) => alive && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      alive = false;
    };
  }, [reloadKey]);

  const lists: ToolLists = useMemo(
    () => ({ allowed: allowedTools, denied: deniedTools, approval: approvalRequiredTools, actionRules }),
    [allowedTools, deniedTools, approvalRequiredTools, actionRules],
  );

  const shown = useMemo(() => (tools ? visibleTools(tools, lists) : []), [tools, lists]);

  // Draft resolved against the CURRENT baseline: an entry that no longer
  // differs (e.g. 進階設定 made the same edit) simply drops out.
  const pending = useMemo(() => {
    const out = new Map<string, Desired>();
    for (const tool of shown) {
      const d = draft.get(tool.qualified);
      if (!d) continue;
      const base = rowState(tool, lists);
      const full = { allowed: d.allowed ?? base.allowed, approval: d.approval ?? base.approval };
      if (full.allowed !== base.allowed || full.approval !== base.approval) out.set(tool.qualified, full);
    }
    return out;
  }, [draft, shown, lists]);

  const summary = useMemo(() => summarize(shown, lists, pending), [shown, lists, pending]);
  const changedKeys = useMemo(() => new Set(summary.items.map((i) => i.tool.qualified)), [summary]);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = q
      ? shown.filter(
          (e) =>
            e.name.toLowerCase().includes(q) ||
            e.qualified.toLowerCase().includes(q) ||
            e.description.toLowerCase().includes(q),
        )
      : shown;
    const byCat = new Map<string, BuiltinToolEntry[]>();
    for (const e of filtered) {
      const arr = byCat.get(e.category) ?? [];
      arr.push(e);
      byCat.set(e.category, arr);
    }
    return Array.from(byCat.entries());
  }, [shown, query]);

  const effective = useCallback(
    (tool: BuiltinToolEntry) => {
      const base = rowState(tool, lists);
      const d = pending.get(tool.qualified);
      return { ...base, allowed: d?.allowed ?? base.allowed, approval: d?.approval ?? base.approval };
    },
    [lists, pending],
  );

  const patch = useCallback((qualified: string, change: Partial<Desired>) => {
    setDraft((prev) => {
      const next = new Map(prev);
      next.set(qualified, { ...(prev.get(qualified) ?? {}), ...change });
      return next;
    });
  }, []);

  const catLabel = (cat: string) => intl.formatMessage({ id: `agents.cap.toolCat.${cat}`, defaultMessage: cat });

  const readOnly = !isAdmin;
  const unsaved = summary.items.length;

  const confirm = () => {
    if (!isAdmin) return;
    const next = computeNextLists(shown, lists, pending);
    const delta = changedLists(lists, next);
    setConfirmOpen(false);
    setDraft(new Map());
    if (Object.keys(delta).length > 0) onApply(delta);
  };

  const stateText = (allowed: boolean, approval: boolean) =>
    !allowed ? t('agents.perm.state.denied') : approval ? t('agents.perm.state.approval') : t('agents.perm.state.allowed');

  if (error) {
    return (
      <div role="alert" className="space-y-2 rounded-lg border border-border px-4 py-6 text-center">
        <p className="text-sm font-medium text-foreground">{t('agents.perm.matrix.error.title')}</p>
        <p className="text-xs text-muted-foreground">{t('agents.perm.matrix.error.desc')}</p>
        <Button variant="outline" size="sm" onClick={() => setReloadKey((k) => k + 1)}>
          {t('agents.perm.matrix.retry')}
        </Button>
      </div>
    );
  }
  if (!tools) {
    return <p className="px-2 py-6 text-center text-xs text-muted-foreground">{t('agents.perm.matrix.loading')}</p>;
  }

  return (
    <div className="space-y-3">
      {readOnly && (
        <p className="flex items-start gap-2 rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
          <Lock className="mt-0.5 size-3.5 shrink-0" />
          {t('agents.perm.matrix.readonly')}
        </p>
      )}
      <div className="relative">
        <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t('agents.perm.matrix.search')}
          aria-label={t('agents.perm.matrix.search.aria')}
          className={cn(inputClass, 'pl-8')}
        />
      </div>

      <div className="overflow-hidden rounded-lg border border-border">
        <div className="grid grid-cols-[1fr_5.5rem_5.5rem] border-b border-border bg-muted/40 px-3 py-2 text-xs font-medium text-muted-foreground">
          <span>{t('agents.perm.matrix.col.tool')}</span>
          <span className="text-center">{t('agents.perm.matrix.col.use')}</span>
          <span className="text-center">{t('agents.perm.matrix.col.approval')}</span>
        </div>
        {rows.length === 0 && (
          <p className="px-3 py-6 text-center text-xs text-muted-foreground">{t('agents.perm.matrix.empty')}</p>
        )}
        {rows.map(([cat, entries], idx) => {
          const open = query.trim()
            ? true
            : openedGroups.has(cat) || (idx < INITIAL_OPEN_GROUPS && !closedGroups.has(cat));
          const states = entries.map((e) => effective(e));
          const allowedCount = states.filter((s) => s.allowed).length;
          const togglable = entries.filter((_, i) => !(states[i].allowLocked && !states[i].allowed));
          const allOn = togglable.length > 0 && togglable.every((e) => effective(e).allowed);
          const someOn = togglable.some((e) => effective(e).allowed);
          const flip = () => {
            for (const e of togglable) patch(e.qualified, { allowed: !allOn });
          };
          return (
            <div key={cat} className="border-b border-border last:border-b-0">
              <div className="grid grid-cols-[1fr_5.5rem_5.5rem] items-center bg-muted/20 px-3 py-1.5">
                <button
                  type="button"
                  aria-expanded={open}
                  onClick={() => {
                    if (query.trim()) return;
                    const setter = open ? setClosedGroups : setOpenedGroups;
                    const other = open ? setOpenedGroups : setClosedGroups;
                    setter((p) => new Set(p).add(cat));
                    other((p) => {
                      const n = new Set(p);
                      n.delete(cat);
                      return n;
                    });
                  }}
                  className="flex items-center gap-1.5 text-left text-xs font-semibold text-foreground"
                >
                  <ChevronRight className={cn('size-3.5 transition-transform', open && 'rotate-90')} />
                  {catLabel(cat)}
                  <span className="font-normal text-muted-foreground">
                    {t('agents.perm.matrix.group.count', { n: allowedCount, m: entries.length })}
                  </span>
                </button>
                <span className="flex justify-center">
                  {!readOnly && (
                    <Checkbox
                      checked={allOn}
                      indeterminate={someOn && !allOn}
                      onCheckedChange={flip}
                      aria-label={t('agents.perm.matrix.group.selectAll', { group: catLabel(cat) })}
                    />
                  )}
                </span>
                <span />
              </div>
              {open &&
                entries.map((tool, i) => {
                  const s = states[i];
                  const changed = changedKeys.has(tool.qualified);
                  const useLocked = readOnly || (s.allowLocked && !s.allowed);
                  const approvalOff = !s.allowed;
                  const approvalDisabled = readOnly || approvalOff || (s.approvalLocked && s.approval);
                  return (
                    <div
                      key={tool.qualified}
                      data-changed={changed || undefined}
                      className={cn(
                        'grid grid-cols-[1fr_5.5rem_5.5rem] items-center border-t border-border/60 px-3 py-2',
                        changed && 'bg-primary/5',
                      )}
                    >
                      <div className="min-w-0 pr-2">
                        <div className="flex items-center gap-1.5">
                          <span className="truncate font-mono text-xs font-medium text-foreground">{tool.name}</span>
                          {changed && (
                            <Badge variant="secondary" className="shrink-0 px-1 py-0 text-[10px]">
                              {t('agents.perm.matrix.changed')}
                            </Badge>
                          )}
                        </div>
                        <p className="truncate text-[11px] text-muted-foreground">{tool.description}</p>
                      </div>
                      <span
                        className="flex justify-center"
                        title={s.allowLocked && !s.allowed ? t('agents.perm.matrix.allowLocked') : undefined}
                      >
                        <Checkbox
                          checked={s.allowed}
                          disabled={useLocked}
                          onCheckedChange={(v) => patch(tool.qualified, { allowed: Boolean(v) })}
                          aria-label={t('agents.perm.matrix.aria.use', { tool: tool.name })}
                        />
                      </span>
                      <span
                        className="flex justify-center"
                        title={
                          approvalOff
                            ? t('agents.perm.matrix.approvalDisabled')
                            : s.approvalLocked && s.approval
                              ? t('agents.perm.matrix.approvalLocked')
                              : undefined
                        }
                      >
                        <Checkbox
                          checked={s.approval}
                          disabled={approvalDisabled}
                          onCheckedChange={(v) => patch(tool.qualified, { approval: Boolean(v) })}
                          aria-label={t('agents.perm.matrix.aria.approval', { tool: tool.name })}
                        />
                      </span>
                    </div>
                  );
                })}
            </div>
          );
        })}
      </div>

      {!readOnly && unsaved > 0 && (
        <div className="sticky bottom-3 z-10 flex items-center justify-between gap-3 rounded-lg border border-border bg-surface-raised px-3 py-2 shadow-md">
          <span className="text-xs text-foreground">{t('agents.perm.matrix.unsaved', { n: unsaved })}</span>
          <span className="flex gap-2">
            <Button variant="ghost" size="sm" onClick={() => setDraft(new Map())}>
              {t('agents.perm.matrix.discard')}
            </Button>
            <Button size="sm" onClick={() => setConfirmOpen(true)}>
              {t('agents.perm.matrix.save')}
            </Button>
          </span>
        </div>
      )}

      <Dialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{t('agents.perm.confirm.title')}</DialogTitle>
            <DialogDescription>{t('agents.perm.confirm.desc', { agent: agentName })}</DialogDescription>
          </DialogHeader>
          <div className="flex flex-wrap gap-2 text-xs">
            <Badge variant="secondary">{t('agents.perm.confirm.added', { n: summary.addedAllowed })}</Badge>
            <Badge variant="secondary">{t('agents.perm.confirm.removed', { n: summary.removedAllowed })}</Badge>
            <Badge variant="secondary">{t('agents.perm.confirm.approval', { n: summary.approvalChanged })}</Badge>
          </div>
          <ul className="max-h-64 space-y-1.5 overflow-y-auto text-xs">
            {summary.items.map(({ tool, allowed, approval }) => {
              const s = effective(tool);
              const base = rowState(tool, lists);
              return (
                <li key={tool.qualified} className="rounded-md bg-muted/40 px-2.5 py-1.5">
                  <span className="font-mono font-medium text-foreground">{tool.name}</span>{' '}
                  <span className="text-muted-foreground">
                    {stateText(base.allowed, base.approval)} → {stateText(s.allowed, s.approval)}
                  </span>
                  <span className="sr-only">
                    {allowed ? (allowed.to ? t('agents.perm.confirm.tag.added') : t('agents.perm.confirm.tag.removed')) : ''}
                    {approval ? t('agents.perm.confirm.tag.approval') : ''}
                  </span>
                </li>
              );
            })}
          </ul>
          <p className="text-xs text-muted-foreground">{t('agents.perm.confirm.note')}</p>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setConfirmOpen(false)}>
              {t('agents.perm.confirm.back')}
            </Button>
            <Button onClick={confirm}>{t('agents.perm.confirm.ok')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
