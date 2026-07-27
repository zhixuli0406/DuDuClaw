import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  FileTextIcon,
  MessageSquareIcon,
  BrainIcon,
  BookOpenIcon,
  ClockIcon,
  UsersIcon,
  PuzzleIcon,
  MailIcon,
  StickyNoteIcon,
  GithubIcon,
  BriefcaseIcon,
  MonitorIcon,
  TerminalIcon,
  SettingsIcon,
  type LucideIcon,
} from 'lucide-react';
import { type BuiltinToolEntry } from '@/lib/api';
import { Badge, Button, Switch, SettingsCard, SettingsRow } from '@/components/mds';
import { fetchCatalog } from './ToolCatalogPicker';

/**
 * CapabilityToggles — plain-language feature switches over the built-in tool
 * catalog, for operators without an engineering background.
 *
 * Semantics ("deny-compile"): the platform default is "empty allowlist = every
 * tool available", so a group toggled OFF compiles to that group's qualified
 * tool names appended to `denied_tools`; toggled ON removes them. The raw
 * `allowed_tools` / `denied_tools` editors stay available under the page's
 * advanced section — when an explicit allowlist is set there, these switches
 * step aside (banner + disabled) because allowlist mode overrides them.
 */

/** UI feature groups: catalog categories folded into everyday-language units. */
const GROUPS: ReadonlyArray<{ id: string; cats: readonly string[]; icon: LucideIcon; caution?: boolean }> = [
  { id: 'office', cats: ['office'], icon: FileTextIcon },
  { id: 'channel', cats: ['channel'], icon: MessageSquareIcon },
  { id: 'memory', cats: ['memory'], icon: BrainIcon },
  { id: 'knowledge', cats: ['wiki', 'identity'], icon: BookOpenIcon },
  { id: 'schedule', cats: ['cron'], icon: ClockIcon },
  { id: 'collab', cats: ['agent'], icon: UsersIcon, caution: true },
  { id: 'skills', cats: ['skill'], icon: PuzzleIcon },
  { id: 'google', cats: ['google'], icon: MailIcon },
  { id: 'notion', cats: ['notion'], icon: StickyNoteIcon },
  { id: 'github', cats: ['github'], icon: GithubIcon },
  { id: 'odoo', cats: ['odoo'], icon: BriefcaseIcon },
  { id: 'desktop', cats: ['os'], icon: MonitorIcon },
  { id: 'coding', cats: ['claude'], icon: TerminalIcon, caution: true },
  // Catch-all: system/inference/security/fork plus any category the catalog
  // grows later that this map doesn't know yet.
  { id: 'system', cats: ['system', 'inference', 'security', 'fork'], icon: SettingsIcon, caution: true },
];

const KNOWN_CATS = new Set(GROUPS.flatMap((g) => g.cats));

/**
 * Base tool name for matching — mirrors the gateway's `tool_base_name`: strip
 * an optional `(qualifier)` and an `mcp__<server>__` namespace prefix so
 * `mcp__duduclaw__office_script`, `office_script` and `Bash(git:*)` all
 * compare on their bare token.
 */
function baseName(entry: string): string {
  const e = (entry.split('(')[0] ?? entry).trim();
  if (e.startsWith('mcp__')) {
    const rest = e.slice(5);
    const idx = rest.indexOf('__');
    if (idx >= 0) {
      const bare = rest.slice(idx + 2);
      if (bare) return bare;
    }
  }
  return e;
}

interface CapabilityTogglesProps {
  /** Current `[capabilities] denied_tools` (qualified or bare entries). */
  deniedTools: ReadonlyArray<string>;
  /** Current `[capabilities] allowed_tools`; non-empty = allowlist mode. */
  allowedTools: ReadonlyArray<string>;
  /** Replace `denied_tools` with a fresh array. */
  onDeniedChange: (next: string[]) => void;
  /** Clear the allowlist (leave allowlist mode). */
  onClearAllowlist: () => void;
}

export function CapabilityToggles({
  deniedTools,
  allowedTools,
  onDeniedChange,
  onClearAllowlist,
}: CapabilityTogglesProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const [tools, setTools] = useState<BuiltinToolEntry[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    fetchCatalog()
      .then((list) => {
        if (alive) setTools(list);
      })
      .catch((e) => {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      alive = false;
    };
  }, []);

  const groups = useMemo(() => {
    return GROUPS.map((g) => {
      const members = tools.filter(
        (e) =>
          g.cats.includes(e.category) ||
          (g.id === 'system' && !KNOWN_CATS.has(e.category)),
      );
      return { ...g, members };
    }).filter((g) => g.members.length > 0);
  }, [tools]);

  const deniedBase = useMemo(
    () => new Set(deniedTools.map(baseName)),
    [deniedTools],
  );

  const allowlistMode = allowedTools.length > 0;

  const toggleGroup = (memberEntries: BuiltinToolEntry[], on: boolean) => {
    const memberBases = new Set(memberEntries.map((e) => baseName(e.qualified)));
    if (on) {
      onDeniedChange(deniedTools.filter((d) => !memberBases.has(baseName(d))));
    } else {
      const additions = memberEntries
        .filter((e) => !deniedBase.has(baseName(e.qualified)))
        .map((e) => e.qualified);
      onDeniedChange([...deniedTools, ...additions]);
    }
  };

  if (error) {
    return (
      <p className="rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">
        {t('agents.cap.toolPicker.error')}: {error}
      </p>
    );
  }

  return (
    <div className="space-y-3">
      {allowlistMode && (
        <div className="flex flex-col gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-xs text-amber-700 dark:text-amber-400">
            {t('agents.cap.toggles.allowlistBanner')}
          </p>
          <Button variant="outline" size="sm" onClick={onClearAllowlist}>
            {t('agents.cap.toggles.clearAllowlist')}
          </Button>
        </div>
      )}
      <SettingsCard>
        {groups.map((g) => {
          const deniedCount = g.members.filter((e) =>
            deniedBase.has(baseName(e.qualified)),
          ).length;
          const off = deniedCount === g.members.length;
          const partial = deniedCount > 0 && !off;
          const Icon = g.icon;
          return (
            <SettingsRow
              key={g.id}
              label={
                <span className="flex items-center gap-2">
                  <Icon className="size-4 shrink-0 text-muted-foreground" />
                  {t(`agents.cap.group.${g.id}`)}
                  {g.caution && (
                    <Badge variant="outline" className="border-amber-500/40 px-1.5 py-0 text-[10px] text-amber-600 dark:text-amber-400">
                      {t('agents.cap.toggles.caution')}
                    </Badge>
                  )}
                  {partial && (
                    <Badge variant="secondary" className="px-1.5 py-0 text-[10px]">
                      {t('agents.cap.toggles.partial')}
                    </Badge>
                  )}
                </span>
              }
              description={t(`agents.cap.group.${g.id}.desc`)}
            >
              <Switch
                checked={!off}
                disabled={allowlistMode}
                onCheckedChange={(v) => toggleGroup(g.members, Boolean(v))}
                aria-label={t(`agents.cap.group.${g.id}`)}
              />
            </SettingsRow>
          );
        })}
      </SettingsCard>
    </div>
  );
}
