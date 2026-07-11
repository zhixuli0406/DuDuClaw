import type { ComponentType } from 'react';
import {
  Sparkles,
  CircleCheckBig,
  Trophy,
  BookOpen,
  Wand2,
  Inbox,
  Rocket,
  Clock,
} from 'lucide-react';

/**
 * Front-end presentation map for the gateway's achievement ids (§6.3). The
 * gateway is the single source of unlock/progress truth and only sends ids;
 * this table maps each id to an icon + i18n name/description key. Ids the
 * gateway emits but this table doesn't know still render (id as fallback name),
 * so a new backend achievement never crashes the wall.
 *
 * Mirrors the id set in W5-BE (`growth.rs`):
 *   first_agent · first_task_done · tasks_100 · knowledge_100 · skills_10 ·
 *   inbox_zero_streak_7 · custom_skill_first · custom_skill_saved_100h
 */
export interface AchievementDef {
  nameId: string;
  descId: string;
  icon: ComponentType<{ className?: string }>;
}

export const ACHIEVEMENT_DEFS: Record<string, AchievementDef> = {
  first_agent: {
    nameId: 'growth.ach.first_agent.name',
    descId: 'growth.ach.first_agent.desc',
    icon: Sparkles,
  },
  first_task_done: {
    nameId: 'growth.ach.first_task_done.name',
    descId: 'growth.ach.first_task_done.desc',
    icon: CircleCheckBig,
  },
  tasks_100: {
    nameId: 'growth.ach.tasks_100.name',
    descId: 'growth.ach.tasks_100.desc',
    icon: Trophy,
  },
  knowledge_100: {
    nameId: 'growth.ach.knowledge_100.name',
    descId: 'growth.ach.knowledge_100.desc',
    icon: BookOpen,
  },
  skills_10: {
    nameId: 'growth.ach.skills_10.name',
    descId: 'growth.ach.skills_10.desc',
    icon: Wand2,
  },
  inbox_zero_streak_7: {
    nameId: 'growth.ach.inbox_zero_streak_7.name',
    descId: 'growth.ach.inbox_zero_streak_7.desc',
    icon: Inbox,
  },
  custom_skill_first: {
    nameId: 'growth.ach.custom_skill_first.name',
    descId: 'growth.ach.custom_skill_first.desc',
    icon: Rocket,
  },
  custom_skill_saved_100h: {
    nameId: 'growth.ach.custom_skill_saved_100h.name',
    descId: 'growth.ach.custom_skill_saved_100h.desc',
    icon: Clock,
  },
};
