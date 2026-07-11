/**
 * Agent staff面 components (dashboard-redesign-v2 §5.4 / V6). The character-card
 * roster, the detail hero + overview, and the org-node side panel. Import from
 * '@/components/agent'.
 */
export { RosterCard } from './RosterCard';
export { HireSlotCard } from './HireSlotCard';
export { AgentHero } from './AgentHero';
export { AgentOverviewTab } from './AgentOverviewTab';
export { OrgNodePanel } from './OrgNodePanel';
export {
  agentTaskStats,
  staffLevel,
  isLiveState,
  isSameLocalDay,
  XP_PER_DONE_TASK,
  type AgentTaskStats,
} from './agent-stats';
