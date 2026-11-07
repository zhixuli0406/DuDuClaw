import { TASK_ENTRY_POINTS_HIDDEN } from '@/lib/task-entry-visibility';
import { client } from '@/lib/ws-client';

const SETUP_READ_TIMEOUT_MS = 10_000;

/** Null means the read failed; false is a successful read of an unfinished item. */
export interface SetupData {
  company: string | null | undefined;
  knowledge: number | null | undefined;
  runtime: boolean | null | undefined;
  accounts: number | null | undefined;
  google: boolean | null | undefined;
  odoo: boolean | null | undefined;
  agents: number | null | undefined;
  users: { role: string; status: string; bindings: unknown[] }[] | null | undefined;
  channels: boolean | null | undefined;
  skills: number | null | undefined;
  routines: number | null | undefined;
  tasks: number | null | undefined;
}
export type SetupSection = 'company' | 'people' | 'capabilities' | 'start';
export interface SetupItem {
  id: string;
  label: string;
  to: string;
  section: SetupSection;
  optional?: boolean;
  check: (data: SetupData) => boolean | null | undefined;
}
const positive = (count: number | null | undefined) => count == null ? count : count > 0;

/** One table owns the checks, grouping, destinations and progress membership. */
export const setupItems: readonly SetupItem[] = [
  { id: 'company', label: 'setup.companyName', to: '/app/system/settings/company', section: 'company', check: d => d.company == null ? d.company : d.company.trim().length > 0 },
  { id: 'knowledge', label: 'setup.knowledge', to: '/knowledge-base', section: 'company', check: d => positive(d.knowledge) },
  { id: 'accounts', label: 'setup.accounts', to: '/app/system/accounts', section: 'company', check: d => d.accounts === undefined || d.accounts === null ? null : d.accounts > 0 },
  { id: 'google', label: 'setup.google', to: '/manage/integrations?tab=google', section: 'company', optional: true, check: d => d.google },
  { id: 'odoo', label: 'setup.odoo', to: '/odoo', section: 'company', optional: true, check: d => d.odoo },
  { id: 'agents', label: 'setup.agents', to: '/agents', section: 'people', check: d => positive(d.agents) },
  { id: 'users', label: 'setup.users', to: '/manage/users', section: 'people', check: d => d.users == null ? d.users : d.users.filter(u => u.status === 'active').length >= 2 },
  { id: 'channels', label: 'setup.channels', to: '/manage/channels', section: 'people', check: d => d.channels },
  { id: 'permissions', label: 'setup.permissions', to: '/manage/users', section: 'people', check: d => d.users == null ? d.users : d.users.filter(u => u.role === 'employee').every(u => u.bindings.length > 0) },
  { id: 'skills', label: 'setup.skills', to: '/skills', section: 'capabilities', check: d => positive(d.skills) },
  { id: 'routines', label: 'setup.routines', to: '/routines', section: 'capabilities', optional: true, check: d => positive(d.routines) },
  ...(!TASK_ENTRY_POINTS_HIDDEN ? [{ id: 'tasks', label: 'setup.tasks', to: '/assign', section: 'start' as const, check: (d: SetupData) => positive(d.tasks) }] : []),
];
export const setupSections: readonly SetupSection[] = ['company', 'people', 'capabilities', ...(!TASK_ENTRY_POINTS_HIDDEN ? ['start' as const] : [])];
export function setupProgress(data: SetupData) {
  const required = setupItems.filter(item => !item.optional);
  return { done: required.filter(item => item.check(data) === true).length, total: required.length };
}
async function read<T>(method: string, normalize: (value: unknown) => T): Promise<T | null> {
  try { return normalize(await client.call(method, {}, false, SETUP_READ_TIMEOUT_MS)); } catch { return null; }
}
const record = (value: unknown): Record<string, any> => {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid response');
  return value as Record<string, any>;
};
export type SetupDataProgress = (key: keyof SetupData, value: SetupData[keyof SetupData]) => void;
export async function loadSetupData(onProgress?: SetupDataProgress): Promise<SetupData> {
  // Normalize inside the guarded reads: an incompatible response is unknown,
  // just like a rejected request, and cannot reject the whole checklist.
  const data: SetupData = {
    company: undefined, knowledge: undefined, runtime: undefined, accounts: undefined,
    google: undefined, odoo: undefined, agents: undefined, users: undefined,
    channels: undefined, skills: undefined, routines: undefined, tasks: undefined,
  };
  const collect = <K extends keyof SetupData>(key: K, promise: Promise<SetupData[K]>) => promise.then(value => {
    data[key] = value;
    onProgress?.(key, value);
  });
  await Promise.all([
    collect('company', read('company.get', value => {
      const company = record(value);
      if (typeof company.name !== 'string') throw new Error('Invalid company response');
      return company.name;
    })),
    collect('knowledge', read('knowledge.collections.list', value => record(value).collections.length)),
    collect('runtime', read('runtime.detect', value => {
      const result = record(value);
      if (typeof result.claude_oauth !== 'boolean') throw new Error('Invalid runtime response');
      return result.claude_oauth || result.claude_account_ready === true;
    })),
    collect('accounts', read('accounts.list', value => record(value).accounts.length)),
    collect('google', read('google.delegation.get', value => {
      const result = record(value);
      return result.personal_connections == null ? null : result.personal_connections.some((connection: { status?: string }) => connection.status === 'connected');
    })),
    collect('odoo', read('odoo.status', value => {
      const result = record(value);
      if (typeof result.connected !== 'boolean') throw new Error('Invalid Odoo response');
      return result.connected;
    })),
    collect('agents', read('agents.list', value => record(value).agents.filter((agent: { archived?: boolean }) => !agent.archived).length)),
    collect('users', read('users.list', value => {
      const result = record(value);
      if (!Array.isArray(result.users) || result.users.some(user => !Array.isArray(user.bindings))) throw new Error('Invalid users response');
      return result.users;
    })),
    collect('channels', read('channels.status', value => record(value).channels.some((channel: { connected?: boolean }) => channel.connected))),
    collect('skills', read('skills.list', value => {
      const result = record(value);
      return result.global_skills.length + result.agents.reduce((n: number, agent: { skills: unknown[] }) => n + agent.skills.length, 0);
    })),
    collect('routines', read('cron.list', value => record(value).tasks.length)),
    collect('tasks', read('tasks.list', value => record(value).tasks.length)),
  ]);
  return data;
}
