import { useState, useEffect, useCallback, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Sparkles, Power, PowerOff, Archive, Settings2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { api, type SkillInfo, type SkillScanPath, type SkillStateStatus } from '@/lib/api';
import type { SkillTarget } from '@/lib/api-skills-manage';
import { client } from '@/lib/ws-client';
import { debounceTrailing } from '@/lib/debounce';
import { matchesQuery, sortSkills } from '@/lib/skill-library';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { toast, formatError } from '@/lib/toast';
import { CustomSkillsSection } from '@/components/skills/CustomSkillsSection';
import {
  CollectionPageState,
  Button,
  buttonVariants,
  Checkbox,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
} from '@/components/mds';
import { AgentSelect, ALL_AGENTS } from './AgentSelect';
import { SkillSortSelect } from './SkillSortSelect';
import { useSkillSort } from './use-skill-sort';
import { useSkillActions } from './use-skill-actions';
import { InstalledSkillRow, MY_SKILLS_COLUMNS } from './InstalledSkillRow';
import { SkillUsageDialog, type HolderRow } from './SkillUsageDialog';
import { RemoveSkillsDialog } from './RemoveSkillsDialog';
import { SkillStateErrors } from './SkillStateErrors';
import { isRemovable, rowKey, searchFieldsOf, sortFieldsOf, switchTargetsOf } from './skill-rows';

/**
 * The scanned-paths block the empty state renders: the directories the
 * gateway actually walked, with their per-layer counts, so "no skills exist"
 * and "the folder being read is not the folder skills were written to" look
 * different on screen.
 */
function ScanPathList({ scanned }: { scanned: SkillScanPath[] }) {
  const intl = useIntl();
  if (scanned.length === 0) return null;
  const seen = new Set<string>();
  const unique = scanned.filter((s) => {
    if (seen.has(s.path)) return false;
    seen.add(s.path);
    return true;
  });
  return (
    <div className="mt-4 space-y-1 text-left">
      <p className="text-xs font-medium text-muted-foreground">
        {intl.formatMessage({ id: 'skills.my.scannedPaths' })}
      </p>
      <ul className="space-y-0.5">
        {unique.map((s) => (
          <li key={s.path} className="flex items-center gap-2 font-mono text-xs text-muted-foreground">
            <span
              className={cn(
                'size-1.5 shrink-0 rounded-full',
                s.exists ? (s.count > 0 ? 'bg-success' : 'bg-warning') : 'bg-muted-foreground',
              )}
            />
            <span className="truncate" title={s.path}>{s.path}</span>
            <span className="shrink-0 tabular-nums">
              {s.exists
                ? intl.formatMessage({ id: 'skills.my.scanCount' }, { count: s.count })
                : intl.formatMessage({ id: 'skills.my.scanMissing' })}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

interface Loaded {
  rows: SkillInfo[];
  scanned: SkillScanPath[];
  states: Record<string, SkillStateStatus>;
  canManage: Record<string, boolean>;
  usageError: string | null;
}

const EMPTY: Loaded = { rows: [], scanned: [], states: {}, canManage: {}, usageError: null };

async function fetchSkills(agent: string): Promise<Loaded> {
  if (agent === ALL_AGENTS) {
    const r = await api.skills.listAll();
    const states: Record<string, SkillStateStatus> = {};
    const canManage: Record<string, boolean> = {};
    for (const a of r?.agents ?? []) {
      if (a.skill_state) states[a.agent_id] = a.skill_state;
      canManage[a.agent_id] = a.can_manage === true;
    }
    return {
      rows: [
        ...(r?.global_skills ?? []).map((s) => ({ ...s, scope: 'global' as const, agent_id: null })),
        ...(r?.agents ?? []).flatMap((a) => (a.skills ?? []).map((s) => ({ ...s, agent_id: s.agent_id ?? a.agent_id }))),
      ],
      scanned: r?.scanned ?? [],
      states,
      canManage,
      usageError: r?.usage_error ?? null,
    };
  }
  const r = await api.skills.list(agent);
  return {
    rows: r?.skills ?? [],
    scanned: r?.scanned ?? [],
    states: r?.skill_state ? { [agent]: r.skill_state } : {},
    canManage: { [agent]: r?.can_manage === true },
    usageError: r?.usage_error ?? null,
  };
}

/** Installed skills across employees: search, sort, usage, per-employee switches, removal. */
export function MySkillsTab({ filter }: { filter: string }) {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const { agents, fetchAgents } = useAgentsStore();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  // Defaults to the aggregate view (the first agent of the gateway's map
  // order was unrelated to the staffer the customer talks to).
  const [selectedAgent, setSelectedAgent] = useState<string>(ALL_AGENTS);
  const [data, setData] = useState<Loaded>(EMPTY);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [shareSuccess, setShareSuccess] = useState<string | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [usageFor, setUsageFor] = useState<SkillInfo | null>(null);
  const [removeTargets, setRemoveTargets] = useState<SkillTarget[] | null>(null);
  const [sort, setSort] = useSkillSort('installed');

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  const displayNameOf = useCallback(
    (name: string) => agents.find((a) => a.name === name)?.display_name || name,
    [agents],
  );

  const loadSkills = useCallback(
    async (showSpinner: boolean) => {
      if (showSpinner) setLoading(true);
      try {
        setData(await fetchSkills(selectedAgent));
        setError(null);
      } catch (e) {
        console.warn('[api]', e);
        // Inline as well as a toast: an empty list would otherwise read as
        // "you have no skills", a different and wrong statement.
        setError(formatError(e));
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
        setData(EMPTY);
      } finally {
        if (showSpinner) setLoading(false);
      }
    },
    [selectedAgent, intl],
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setSelected(new Set());
    void loadSkills(true);
  }, [loadSkills, connectionState]);

  // WP6 — refetch on `skill.changed` for the agent on screen, debounced.
  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    const refresh = debounceTrailing(() => void loadSkills(false));
    const unsubscribe = client.subscribe('skill.changed', (payload) => {
      const p = (payload ?? {}) as { agent_id?: string | null };
      if (selectedAgent !== ALL_AGENTS && p.agent_id && p.agent_id !== selectedAgent) return;
      refresh();
    });
    return () => {
      refresh.cancel();
      unsubscribe();
    };
  }, [selectedAgent, loadSkills, connectionState]);

  const refresh = useCallback(() => void loadSkills(false), [loadSkills]);
  const actions = useSkillActions(refresh);

  const handleShare = useCallback(
    async (skill: SkillInfo) => {
      if (!skill.agent_id) return;
      try {
        await api.sharedSkills.share(skill.agent_id, skill.name);
        // Keyed by row (owner + name), not by name alone.
        setShareSuccess(rowKey(skill));
        setTimeout(() => setShareSuccess(null), 2000);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      }
    },
    [intl],
  );

  const ownerOf = useCallback(
    (s: SkillInfo) => (s.agent_id ? displayNameOf(s.agent_id) : ''),
    [displayNameOf],
  );
  const visible = useMemo(() => {
    const matched = data.rows.filter((s) => matchesQuery(filter, searchFieldsOf(s, ownerOf(s))));
    return sortSkills(matched, sort, intl.locale, (s) => sortFieldsOf(s, ownerOf(s)));
  }, [data.rows, filter, sort, intl.locale, ownerOf]);

  const canManageRow = useCallback(
    (s: SkillInfo) => {
      const targets = switchTargetsOf(s);
      return targets.length > 0 && targets.every((t) => data.canManage[t.agent_id] === true);
    },
    [data.canManage],
  );

  const holdersOf = useCallback(
    (skill: SkillInfo): HolderRow[] => {
      const fromGlobal = data.rows
        .filter((r) => r.scope === 'global' && r.name === skill.name)
        .flatMap((r) =>
          (r.holders ?? []).map((h) => ({
            agent_id: h.agent_id,
            display_name: h.display_name || displayNameOf(h.agent_id),
            scope: 'global' as const,
            enabled: h.enabled,
            use_count: h.use_count,
            last_used_at: h.last_used_at,
            can_manage: data.canManage[h.agent_id] === true,
          })),
        );
      const own = data.rows
        .filter((r) => r.agent_id && r.name === skill.name)
        .map((r) => ({
          agent_id: r.agent_id as string,
          display_name: displayNameOf(r.agent_id as string),
          scope: r.scope ?? 'agent',
          enabled: r.enabled !== false,
          use_count: r.use_count ?? 0,
          last_used_at: r.last_used_at,
          can_manage: data.canManage[r.agent_id as string] === true,
        }));
      return [...own, ...fromGlobal];
    },
    [data.rows, data.canManage, displayNameOf],
  );

  const selectedRows = useMemo(() => visible.filter((s) => selected.has(rowKey(s))), [visible, selected]);
  const toggleSelected = (key: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });
  const allSelected = visible.length > 0 && selectedRows.length === visible.length;
  const bulkTargets = selectedRows.flatMap(switchTargetsOf);
  const bulkRemovable = selectedRows.filter(isRemovable).map((s) => ({ agent_id: s.agent_id as string, skill_name: s.name }));

  const afterBulk = () => setSelected(new Set());

  return (
    <div className="space-y-6">
      <CustomSkillsSection filter={filter} />

      <section className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'skills.my.installed' })}
          </h2>
          <div className="flex flex-wrap items-center gap-2">
            <SkillSortSelect value={sort} onChange={setSort} />
            <AgentSelect
              value={selectedAgent}
              onValueChange={setSelectedAgent}
              agents={agents}
              placeholder={intl.formatMessage({ id: 'skills.my.selectAgent' })}
              allOption={intl.formatMessage({ id: 'skills.my.allAgents' })}
            />
            {selectedAgent !== ALL_AGENTS && (
              <Link
                to={`/agents/${encodeURIComponent(selectedAgent)}/edit?tab=skills`}
                className={buttonVariants({ variant: 'outline', size: 'sm' })}
              >
                <Settings2 />
                {intl.formatMessage({ id: 'skills.manage.openAgent' })}
              </Link>
            )}
          </div>
        </div>

        <SkillStateErrors states={data.states} nameOf={displayNameOf} />
        {data.usageError && (
          <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'skills.usage.unavailable' })}</p>
        )}

        {isAdmin && selectedRows.length > 0 && (
          <div className="flex flex-wrap items-center gap-2 rounded-lg border border-surface-border bg-surface px-3 py-2 text-sm">
            <span className="text-muted-foreground">
              {intl.formatMessage({ id: 'skills.manage.selected' }, { count: selectedRows.length })}
            </span>
            <Button size="sm" variant="outline" disabled={actions.busy} onClick={() => void actions.setEnabled(bulkTargets, false).then(afterBulk)}>
              <PowerOff />
              {intl.formatMessage({ id: 'skills.manage.disable' })}
            </Button>
            <Button size="sm" variant="outline" disabled={actions.busy} onClick={() => void actions.setEnabled(bulkTargets, true).then(afterBulk)}>
              <Power />
              {intl.formatMessage({ id: 'skills.manage.enable' })}
            </Button>
            <Button size="sm" variant="outline" disabled={actions.busy || bulkRemovable.length === 0} onClick={() => setRemoveTargets(bulkRemovable)}>
              <Archive />
              {intl.formatMessage({ id: 'skills.manage.removeCount' }, { count: bulkRemovable.length })}
            </Button>
          </div>
        )}

        {loading ? (
          <CollectionPageState state="loading" />
        ) : error ? (
          <div className="space-y-2 rounded-xl border border-destructive/30 bg-destructive/10 p-4">
            <p className="text-sm font-medium text-destructive">{intl.formatMessage({ id: 'skills.my.loadError' })}</p>
            <p className="font-mono text-xs break-all text-destructive/80">{error}</p>
            <Button variant="outline" size="sm" onClick={() => void loadSkills(true)}>
              {intl.formatMessage({ id: 'world.error.retry' })}
            </Button>
          </div>
        ) : visible.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={Sparkles}
            title={intl.formatMessage({ id: filter.trim() ? 'skills.market.noResults' : 'skills.my.empty' })}
            description={filter.trim() ? undefined : intl.formatMessage({ id: 'skills.my.emptyHint' })}
            action={filter.trim() ? undefined : <ScanPathList scanned={data.scanned} />}
          />
        ) : (
          <div className="overflow-x-auto rounded-xl border border-surface-border">
            <ListGridContainer
              columns={MY_SKILLS_COLUMNS}
              className="!h-auto [&>[aria-hidden]]:hidden"
              header={
                <ListGridHeader>
                  <ListGridHeaderCell className="gap-2">
                    {isAdmin && (
                      <Checkbox
                        checked={allSelected}
                        indeterminate={selectedRows.length > 0 && !allSelected}
                        onCheckedChange={(v) => setSelected(v === true ? new Set(visible.map(rowKey)) : new Set())}
                        aria-label={intl.formatMessage({ id: 'skills.manage.selectAll' })}
                      />
                    )}
                    {intl.formatMessage({ id: 'skills.my.col.name' })}
                  </ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'skills.my.col.owner' })}</ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'skills.my.col.usage' })}</ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'skills.my.col.security' })}</ListGridHeaderCell>
                  <ListGridHeaderCell aria-hidden />
                </ListGridHeader>
              }
            >
              {visible.map((skill) => {
                const key = rowKey(skill);
                return (
                  <InstalledSkillRow
                    key={key}
                    skill={skill}
                    owner={ownerOf(skill)}
                    shared={shareSuccess === key}
                    canManage={canManageRow(skill)}
                    selectable={isAdmin}
                    selected={selected.has(key)}
                    onSelect={(v) => toggleSelected(key, v)}
                    onShare={() => void handleShare(skill)}
                    onUsage={() => setUsageFor(skill)}
                    onToggle={(on) => void actions.setEnabled(switchTargetsOf(skill), on)}
                    onRemove={() =>
                      setRemoveTargets([{ agent_id: skill.agent_id as string, skill_name: skill.name }])
                    }
                  />
                );
              })}
            </ListGridContainer>
          </div>
        )}
      </section>

      {usageFor && (
        <SkillUsageDialog
          skillName={usageFor.display_name || usageFor.name}
          description={usageFor.description}
          holders={holdersOf(usageFor)}
          busy={actions.busy}
          onToggle={(agentId, on) =>
            void actions.setEnabled([{ agent_id: agentId, skill_name: usageFor.name }], on)
          }
          onClose={() => setUsageFor(null)}
        />
      )}
      {removeTargets && (
        <RemoveSkillsDialog
          targets={removeTargets}
          nameOf={displayNameOf}
          busy={actions.busy}
          onConfirm={() => void actions.remove(removeTargets).then(() => {
            setRemoveTargets(null);
            afterBulk();
          })}
          onClose={() => setRemoveTargets(null)}
        />
      )}
    </div>
  );
}
