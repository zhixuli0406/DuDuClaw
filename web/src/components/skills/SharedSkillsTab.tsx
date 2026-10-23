import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Download, Share2, CheckCircle } from 'lucide-react';
import { api, type SharedSkillInfo } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { toast, formatError } from '@/lib/toast';
import { matchesQuery, sortAgentsByName, sortSkills, type SkillSortKey } from '@/lib/skill-library';
import {
  CollectionPageState,
  Card,
  CardContent,
  Button,
  Badge,
  ActorAvatar,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
} from '@/components/mds';
import { AgentSelect } from './AgentSelect';
import { SkillSortSelect } from './SkillSortSelect';
import { useSkillSort } from './use-skill-sort';

const SHARED_SORTS: readonly SkillSortKey[] = ['name', 'mostUsed', 'recentlyAdded'];

/** Team (shared) skills: search across name, description and tags; sortable. */
export function SharedSkillsTab({ filter }: { filter: string }) {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [sharedSkills, setSharedSkills] = useState<SharedSkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [adoptTarget, setAdoptTarget] = useState<SharedSkillInfo | null>(null);
  const [adoptAgent, setAdoptAgent] = useState('');
  const [adoptSuccess, setAdoptSuccess] = useState<string | null>(null);
  const [sort, setSort] = useSkillSort('shared');

  useEffect(() => {
    fetchAgents();
    (async () => {
      setLoading(true);
      try {
        const result = await api.sharedSkills.list();
        setSharedSkills(result?.skills ?? []);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
        setSharedSkills([]);
      } finally {
        setLoading(false);
      }
    })();
  }, [fetchAgents, intl]);

  const adoptTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (adoptTimerRef.current) clearTimeout(adoptTimerRef.current);
    },
    [],
  );

  const visible = useMemo(() => {
    const matched = sharedSkills.filter((s) =>
      matchesQuery(filter, [s.name, s.description, s.shared_by, ...(s.tags ?? [])]),
    );
    return sortSkills(matched, sort === 'recentlyUsed' ? 'name' : sort, intl.locale, (s) => ({
      name: s.name,
      useCount: s.usage_count,
      addedAt: s.shared_at,
      stableKey: s.name,
    }));
  }, [sharedSkills, filter, sort, intl.locale]);

  const handleAdopt = useCallback(async () => {
    if (!adoptTarget || !adoptAgent) return;
    try {
      await api.sharedSkills.adopt(adoptTarget.name, adoptAgent);
      setAdoptSuccess(intl.formatMessage({ id: 'skills.shared.adoptSuccess' }, { agent: adoptAgent }));
      setSharedSkills((prev) =>
        prev.map((s) =>
          s.name === adoptTarget.name
            ? { ...s, adopted_by: [...s.adopted_by, adoptAgent], usage_count: s.usage_count + 1 }
            : s,
        ),
      );
      adoptTimerRef.current = setTimeout(() => {
        setAdoptTarget(null);
        setAdoptSuccess(null);
      }, 1500);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [adoptTarget, adoptAgent, intl]);

  if (loading) return <CollectionPageState state="loading" />;
  if (sharedSkills.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={Share2}
        title={intl.formatMessage({ id: 'skills.shared.empty' })}
        description={intl.formatMessage({ id: 'skills.shared.subtitle' })}
      />
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'skills.shared.subtitle' })}</p>
        <SkillSortSelect value={sort} onChange={setSort} keys={SHARED_SORTS} />
      </div>
      {visible.length === 0 ? (
        <CollectionPageState
          state="empty"
          icon={Share2}
          title={intl.formatMessage({ id: 'skills.market.noResults' })}
          description={`"${filter}"`}
        />
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {visible.map((skill) => (
            <SharedSkillCard
              key={skill.name}
              skill={skill}
              onAdopt={() => {
                setAdoptTarget(skill);
                setAdoptAgent(sortAgentsByName(agents, intl.locale)[0]?.name ?? '');
                setAdoptSuccess(null);
              }}
            />
          ))}
        </div>
      )}

      <Dialog
        open={adoptTarget !== null}
        onOpenChange={(o) => {
          if (!o) {
            setAdoptTarget(null);
            setAdoptSuccess(null);
          }
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'skills.shared.adopt' })}</DialogTitle>
            <DialogDescription>
              <strong className="text-foreground">{adoptTarget?.name}</strong> →{' '}
              {intl.formatMessage({ id: 'skills.shared.adoptTo' })}
            </DialogDescription>
          </DialogHeader>
          <AgentSelect
            value={adoptAgent}
            onValueChange={setAdoptAgent}
            agents={agents}
            placeholder={intl.formatMessage({ id: 'skills.shared.adoptTo' })}
          />
          {adoptSuccess && (
            <div className="flex items-center gap-2 rounded-lg bg-success/10 px-3 py-2 text-sm text-success">
              <CheckCircle className="size-4 shrink-0" />
              {adoptSuccess}
            </div>
          )}
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>}
            />
            <Button variant="brand" onClick={handleAdopt} disabled={!adoptAgent || !!adoptSuccess}>
              {intl.formatMessage({ id: 'skills.shared.adopt' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function SharedSkillCard({ skill, onAdopt }: { skill: SharedSkillInfo; onAdopt: () => void }) {
  const intl = useIntl();
  return (
    <Card className="gap-3">
      <CardContent className="flex flex-1 flex-col gap-3">
        <h3 className="truncate text-base font-medium text-foreground" title={skill.name}>
          {skill.name}
        </h3>
        <p className="line-clamp-2 text-sm text-muted-foreground">{skill.description || '—'}</p>
        {skill.tags.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {skill.tags.map((tag) => (
              <Badge key={tag} variant="secondary">{tag}</Badge>
            ))}
          </div>
        )}
        <div className="mt-auto space-y-1.5 text-xs text-muted-foreground">
          <div className="flex items-center gap-1.5">
            <ActorAvatar actorType="agent" size="xs" name={skill.shared_by} />
            <span className="truncate">
              {intl.formatMessage({ id: 'skills.shared.sharedBy' })}: {skill.shared_by}
            </span>
          </div>
          <div className="flex items-center gap-1.5">
            <Download className="size-3" />
            <span>
              {intl.formatMessage({ id: 'skills.shared.usageCount' })}:{' '}
              <span className="font-mono tabular-nums">{skill.usage_count}</span>
            </span>
          </div>
        </div>
      </CardContent>
      <div className="border-t border-surface-border px-4 pt-3">
        <Button variant="outline" size="sm" onClick={onAdopt}>
          <Download />
          {intl.formatMessage({ id: 'skills.shared.adopt' })}
        </Button>
      </div>
    </Card>
  );
}
