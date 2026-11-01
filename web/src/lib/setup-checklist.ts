import { api } from '@/lib/api';
import { knowledgeApi } from '@/lib/knowledge-api';
import { parseTomlSubset, tomlStr } from '@/lib/config-toml';

/** Null means the read failed; false is a successful read of an unfinished item. */
export interface SetupData {
  company: string | null;
  knowledge: number | null;
  runtime: boolean | null;
  accounts: number | null;
  google: boolean | null;
  odoo: boolean | null;
  agents: number | null;
  users: { role: string; status: string; bindings: unknown[] }[] | null;
  channels: boolean | null;
  skills: number | null;
  routines: number | null;
  tasks: number | null;
}
export type SetupSection = 'company' | 'people' | 'capabilities' | 'start';
export interface SetupItem {
  id: string;
  label: string;
  to: string;
  section: SetupSection;
  optional?: boolean;
  check: (data: SetupData) => boolean | null;
}
const positive = (count: number | null) => count === null ? null : count > 0;

/** One table owns the checks, grouping, destinations and progress membership. */
export const setupItems: readonly SetupItem[] = [
  { id: 'company', label: 'setup.companyName', to: '/app/system/settings', section: 'company', check: d => d.company === null ? null : d.company.trim().length > 0 },
  { id: 'knowledge', label: 'setup.knowledge', to: '/knowledge-base', section: 'company', check: d => positive(d.knowledge) },
  { id: 'accounts', label: 'setup.accounts', to: '/app/system/accounts', section: 'company', check: d => d.runtime === null || d.accounts === null ? null : d.runtime || d.accounts > 0 },
  { id: 'google', label: 'setup.google', to: '/manage/integrations?tab=google', section: 'company', optional: true, check: d => d.google },
  { id: 'odoo', label: 'setup.odoo', to: '/odoo', section: 'company', optional: true, check: d => d.odoo },
  { id: 'agents', label: 'setup.agents', to: '/agents', section: 'people', check: d => positive(d.agents) },
  { id: 'users', label: 'setup.users', to: '/manage/users', section: 'people', check: d => d.users === null ? null : d.users.filter(u => u.status === 'active').length >= 2 },
  { id: 'channels', label: 'setup.channels', to: '/manage/channels', section: 'people', check: d => d.channels },
  { id: 'permissions', label: 'setup.permissions', to: '/manage/users', section: 'people', check: d => d.users === null ? null : d.users.filter(u => u.role === 'employee').every(u => u.bindings.length > 0) },
  { id: 'skills', label: 'setup.skills', to: '/skills', section: 'capabilities', check: d => positive(d.skills) },
  { id: 'routines', label: 'setup.routines', to: '/routines', section: 'capabilities', optional: true, check: d => positive(d.routines) },
  { id: 'tasks', label: 'setup.tasks', to: '/assign', section: 'start', check: d => positive(d.tasks) },
];
export const setupSections: readonly SetupSection[] = ['company', 'people', 'capabilities', 'start'];
export function setupProgress(data: SetupData) {
  const required = setupItems.filter(item => !item.optional);
  return { done: required.filter(item => item.check(data) === true).length, total: required.length };
}
async function read<T>(call: () => Promise<T>): Promise<T | null> {
  try { return await call(); } catch { return null; }
}
export async function loadSetupData(): Promise<SetupData> {
  // Normalize inside the guarded reads: an incompatible response is unknown,
  // just like a rejected request, and cannot reject the whole checklist.
  const [company, knowledge, runtime, accounts, google, odoo, agents, users, channels, skills, routines, tasks] = await Promise.all([
    read(async () => {
      const config = await api.system.config();
      if (typeof config.config !== 'string') throw new Error('Invalid config response');
      return tomlStr(parseTomlSubset(config.config), 'general', 'name', '');
    }),
    read(async () => (await knowledgeApi.listCollections()).collections.length),
    read(async () => {
      const result = await api.runtime.detect();
      if (typeof result.claude_oauth !== 'boolean') throw new Error('Invalid runtime response');
      return result.claude_oauth || result.claude_account_ready === true;
    }),
    read(async () => (await api.accounts.list()).accounts.length),
    read(async () => {
      const result = await api.googleIdentity.delegation();
      return result.personal_connections == null ? null : result.personal_connections.some(connection => connection.status === 'connected');
    }),
    read(async () => {
      const result = await api.odoo.status();
      if (typeof result.connected !== 'boolean') throw new Error('Invalid Odoo response');
      return result.connected;
    }),
    read(async () => (await api.agents.list()).agents.filter(agent => !agent.archived).length),
    read(async () => {
      const result = await api.users.list();
      if (!Array.isArray(result.users) || result.users.some(user => !Array.isArray(user.bindings))) throw new Error('Invalid users response');
      return result.users;
    }),
    read(async () => (await api.channels.status()).channels.some(channel => channel.connected)),
    read(async () => {
      const result = await api.skills.listAll();
      return result.global_skills.length + result.agents.reduce((n, agent) => n + agent.skills.length, 0);
    }),
    read(async () => (await api.cron.list()).tasks.length),
    read(async () => (await api.tasks.list()).tasks.length),
  ]);
  return { company, knowledge, runtime, accounts, google, odoo, agents, users, channels, skills, routines, tasks };
}
