import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type RecentSkill } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore } from '@/stores/auth-store';
import { Button } from '@/components/mds';

export function selectedSkillNames(value: string): string[] {
  return [...new Set(value.split(/\s+/).filter(token => /^\/[^/\\\s]+$/.test(token)).map(token => token.slice(1)))];
}

export function toggleSkillToken(value: string, name: string): string {
  const token = `/${name}`;
  if (selectedSkillNames(value).includes(name)) {
    return value.split(/(\s+)/).filter(part => part !== token).join('').replace(/ {2,}/g, ' ').trimStart();
  }
  return `${value}${value && !/\s$/.test(value) ? ' ' : ''}${token} `;
}

export function RecentSkillChips({ value, onChange, agentId, assigning = false, disabled = false, onPick }: {
  value: string;
  onChange: (value: string) => void;
  agentId?: string;
  assigning?: boolean;
  disabled?: boolean;
  onPick?: () => void;
}) {
  const intl = useIntl();
  const connection = useConnectionStore(s => s.state);
  const userId = useAuthStore(s => s.user?.id);
  const [loaded, setLoaded] = useState<{ key: string; skills: RecentSkill[] } | null>(null);
  const key = JSON.stringify([userId, agentId, assigning, connection]);
  useEffect(() => {
    if (connection !== 'authenticated') { setLoaded(null); return; }
    let alive = true;
    api.skills.recent(agentId, assigning).then(result => {
      if (alive) setLoaded({ key, skills: result.skills ?? [] });
    }).catch(() => {
      if (alive) setLoaded({ key, skills: [] });
    });
    return () => { alive = false; };
  }, [key, agentId, assigning, connection]);
  const skills = loaded?.key === key ? loaded.skills.slice(0, 5) : [];
  if (skills.length === 0) return null;
  const selected = selectedSkillNames(value);
  return (
    <section aria-label={intl.formatMessage({ id: 'assign.skill.recentUsed' })} className="space-y-2">
      <h3 className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'assign.skill.recentUsed' })}</h3>
      <div className="flex flex-wrap gap-2">
        {skills.map(skill => {
          const active = selected.includes(skill.name);
          return (
            <Button key={skill.name} type="button" variant={active ? 'secondary' : 'outline'} size="sm"
              aria-pressed={active} disabled={disabled}
              onClick={() => { onChange(toggleSkillToken(value, skill.name)); onPick?.(); }}
              className="h-auto max-w-full items-start whitespace-normal py-2 text-left">
              <span className="min-w-0"><span className="block truncate">{skill.display_name || skill.name}</span>{' '}
                {skill.description && <span className="block max-w-64 truncate text-xs text-muted-foreground">{skill.description}</span>}
              </span>
            </Button>
          );
        })}
      </div>
    </section>
  );
}
