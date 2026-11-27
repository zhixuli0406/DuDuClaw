/** Department-management grants shared by the RPC client and navigation. */
export type GrantGroup = 'schedules' | 'automation' | 'skills' | 'cost' | 'egress_review';

export interface DepartmentGrant {
  department: string;
  groups: GrantGroup[];
}

/** Wire fields are optional for older gateways; unknown groups are ignored. */
export interface MeGrantFields {
  dept_grants?: { department: string; groups: string[] }[];
  main_agent_access?: boolean;
}

export interface DeptGrantsMineResult {
  grants: DepartmentGrant[];
  agents: { id: string; display_name: string; groups: GrantGroup[] }[];
  needs_sync: { id: string; display_name: string }[];
}

export function isGrantGroup(group: string): group is GrantGroup {
  return group === 'schedules' || group === 'automation' || group === 'skills'
    || group === 'cost' || group === 'egress_review';
}
