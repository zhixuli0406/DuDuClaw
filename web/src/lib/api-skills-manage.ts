/**
 * Typed RPC wrappers for per-employee skill management (B8): switch a skill
 * off or on for an employee, remove (archive) a skill from an employee's own
 * layer, list and restore removed skills. Mirrors
 * `crates/duduclaw-gateway/src/handlers/skills_manage_rpc.rs`.
 */
import { client } from './ws-client';

export interface SkillTarget {
  agent_id: string;
  skill_name: string;
}

export interface SkillTargetResult extends SkillTarget {
  ok: boolean;
  error?: string | null;
  archive_id?: string;
}

export interface ArchivedSkill {
  archive_id: string;
  skill_name: string;
  archived_at: string;
  archived_by: string;
}

export const skillsManageApi = {
  /** Owner of every named employee (or admin). */
  setEnabled: (items: SkillTarget[], enabled: boolean) =>
    client.call('skills.set_enabled', { items, enabled }) as Promise<{
      results: SkillTargetResult[];
      enabled: boolean;
    }>,
  /** Moves own-layer skills to the archive; Owner of every named employee (or admin). */
  remove: (items: SkillTarget[]) =>
    client.call('skills.remove', { items }) as Promise<{ results: SkillTargetResult[] }>,
  /** Viewer of the employee. */
  archived: (agentId: string) =>
    client.call('skills.archived', { agent_id: agentId }) as Promise<{
      agent_id: string;
      can_restore: boolean;
      archived: ArchivedSkill[];
    }>,
  /** Owner of the employee (or admin). */
  restore: (agentId: string, archiveId: string) =>
    client.call('skills.restore', { agent_id: agentId, archive_id: archiveId }) as Promise<{
      agent_id: string;
      skill_name: string;
      restored: boolean;
    }>,
};

/** Failed items of a bulk result, for one summary message. */
export function failedTargets(results: readonly SkillTargetResult[]): SkillTargetResult[] {
  return results.filter((r) => !r.ok);
}
