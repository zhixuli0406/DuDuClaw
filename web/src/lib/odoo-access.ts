export type OdooPermissionMode = 'engine' | 'follow_odoo_account';
export type OdooAccess = { readable: false; message: string } | {
  readable: true;
  hash: string;
  config: {
    permission_mode: OdooPermissionMode;
    permission_mode_ack: { by: string; at: string } | null;
    company_key_agents: string[];
    company_key_agents_migrated: boolean;
    always_refuse: string[];
    attachment_max_mb: number;
    upgrade_notice_pending: boolean;
  };
  defaults: { always_refuse: string[] };
  fixed_refusals: { code: string; reason: string }[];
  upgrade_notice_pending: boolean;
  employees: { id: string; display_name: string | null; has_own_account: boolean }[];
  follow_mode_impact: { avatars_with_personal_credentials: number; agents_acting_as_requester: number } | null;
  risk_text: { permission_mode: string; always_refuse: string; company_key_agents: string };
};
export interface OdooAccessPatch {
  expected_hash: string;
  permission_mode?: OdooPermissionMode;
  company_key_agents?: string[];
  always_refuse?: string[];
  attachment_max_mb?: number;
  acknowledge_risk: boolean;
  dismiss_upgrade_notice: boolean;
}

export const REFUSE_GROUPS = [
  { key: 'delete', rules: ['unlink:*'] },
  { key: 'system', rules: ['create:ir.*', 'write:ir.*', 'execute:ir.*'] },
  { key: 'users', rules: ['create:res.users', 'write:res.users'] },
  { key: 'groups', rules: ['create:res.groups', 'write:res.groups'] },
  { key: 'legacy', rules: ['method:execute_import', 'method:signal_workflow'] },
] as const;
export const REFUSE_ACTIONS = ['*', 'create', 'write', 'unlink', 'execute', 'activity', 'note', 'message', 'attach', 'method'] as const;

/** Mirrors RefuseRule::parse, including lowercase model segments and ASCII methods. */
export function validRefuseRule(verb: string, target: string): boolean {
  if (target !== target.trim()) return false;
  if (verb === 'method') return /^[A-Za-z0-9_]+$/.test(target);
  return (REFUSE_ACTIONS as readonly string[]).includes(verb) &&
    (target === '*' || /^[a-z0-9_]+(?:\.[a-z0-9_]+)*(?:\.\*)?$/.test(target));
}

export function refuseRows(rules: string[]) {
  const grouped = new Set<string>();
  const rows: { key: string; rules: string[]; group?: string }[] = [];
  for (const group of REFUSE_GROUPS) {
    const present = group.rules.filter(rule => rules.includes(rule));
    if (present.length) {
      rows.push({ key: group.key, rules: present, group: group.key });
      present.forEach(rule => grouped.add(rule));
    }
  }
  for (const rule of rules) if (!grouped.has(rule)) rows.push({ key: rule, rules: [rule] });
  return rows;
}
