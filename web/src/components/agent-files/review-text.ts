import type { IntlShape } from 'react-intl';
import type {
  AgentFileFixedKey,
  AgentFileKey,
  AgentFileSkillKey,
  AgentFileReason,
  AgentFileReasonCategory,
  AgentFileReview,
  AgentFileSaveResponse,
} from '@/lib/agent-files-types';

/**
 * Plain-language helpers shared by the change-review dialog and the inbox
 * card for `agent_file_change` approvals. Server text stays DATA: it is only
 * ever rendered as React text nodes.
 */

export const AGENT_FILE_KEYS: ReadonlyArray<AgentFileFixedKey> = ['soul', 'identity', 'user', 'instructions', 'contract', 'agent_toml'];

const SKILL_PREFIX = 'skill:';

/** On-disk file names, shown small and grey next to the plain name. */
export const AGENT_FILE_NAMES: Readonly<Record<AgentFileFixedKey, string>> = {
  soul: 'SOUL.md',
  identity: 'IDENTITY.md',
  user: 'USER.md',
  instructions: 'INSTRUCTIONS.md',
  contract: 'CONTRACT.toml',
  agent_toml: 'agent.toml',
};

const CATEGORIES: ReadonlySet<string> = new Set<AgentFileReasonCategory>([
  'prompt_injection',
  'secret_request',
  'loosened_redline',
  'contract_conflict',
  'role_drift',
  'hidden_content',
  'secret_in_text',
  'size',
  'format',
  'analysis_unavailable',
  'outside_change',
  'risky_action',
  'existing_risk',
]);

export function isFixedAgentFileKey(v: unknown): v is AgentFileFixedKey {
  return typeof v === 'string' && (AGENT_FILE_KEYS as readonly string[]).includes(v);
}

/** `skill:<path>` with a non-empty path. */
export function isSkillKey(v: unknown): v is AgentFileSkillKey {
  return typeof v === 'string' && v.startsWith(SKILL_PREFIX) && v.length > SKILL_PREFIX.length;
}

export function isAgentFileKey(v: unknown): v is AgentFileKey {
  return isFixedAgentFileKey(v) || isSkillKey(v);
}

/** The skill's name as the employee knows it: the folder of a `SKILL.md`,
 *  otherwise the file name without `.md`. */
export function skillNameOf(file: AgentFileSkillKey): string {
  const parts = file.slice(SKILL_PREFIX.length).split('/');
  const last = parts[parts.length - 1] ?? '';
  if (last.toLowerCase() === 'skill.md' && parts.length > 1) return parts[parts.length - 2] ?? last;
  return last.replace(/\.md$/i, '');
}

/** The on-disk name shown small and grey (`SOUL.md`, `SKILLS/quote/SKILL.md`). */
export function agentFileName(file: AgentFileKey): string {
  return isSkillKey(file) ? `SKILLS/${file.slice(SKILL_PREFIX.length)}` : AGENT_FILE_NAMES[file];
}

/** Plain-language file name (`人格設定`, `技能：quote`); unknown keys come back as-is. */
export function agentFileLabel(intl: IntlShape, file: string): string {
  if (isSkillKey(file)) return intl.formatMessage({ id: 'agentFiles.file.skillNamed' }, { name: skillNameOf(file) });
  return isFixedAgentFileKey(file) ? intl.formatMessage({ id: `agentFiles.file.${file}` }) : file;
}

/** The name shown in the file list and above the editor. Work rules also
 *  carry the industry names people search for (AGENTS.md / Agent.md); every
 *  other place keeps the short plain name. */
export function agentFileHeading(intl: IntlShape, file: string): string {
  if (file === 'instructions') return intl.formatMessage({ id: 'agentFiles.file.instructionsFull' });
  return agentFileLabel(intl, file);
}

/** Localized text for a reason category. An unknown (newer) category falls
 *  back to the server's own sentence so nothing is silently dropped. */
export function reasonCategoryText(intl: IntlShape, category: string, fallback = ''): string {
  if (CATEGORIES.has(category)) return intl.formatMessage({ id: `agentFiles.category.${category}` });
  return fallback || category;
}

/** True when the automatic check did not complete — never a silent pass. */
export function analysisUnavailable(review: AgentFileReview | undefined | null): boolean {
  if (!review) return false;
  if (review.basis === 'rules+ai_unavailable') return true;
  return review.reasons.some((r: AgentFileReason) => r.category === 'analysis_unavailable');
}

/** Whether a save answer needs the review dialog. `applied` only does when
 *  the automatic check did not finish (the person must be told). An answer
 *  without `status` (an older gateway) counts as applied. */
export function responseNeedsDialog(res: Pick<AgentFileSaveResponse, 'status' | 'review'> | null | undefined): boolean {
  const status = res?.status ?? 'applied';
  if (status === 'unchanged') return false;
  if (status === 'applied') return analysisUnavailable(res?.review);
  return true;
}

/** Short form of an approval id for display (`apr-1234…`). */
export function shortApprovalId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id;
}
