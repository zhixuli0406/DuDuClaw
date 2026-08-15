/**
 * Task components (dashboard-redesign-v2 V5). The dual-view board + the
 * paperclip-style detail page compose from these. Import from '@/components/task'.
 */
export { AssigneePopover, type AssigneeOption } from './AssigneePopover';
export { CreateTaskModal } from './CreateTaskModal';
export { TaskProperties } from './TaskProperties';
export { TaskBottomTabs } from './TaskBottomTabs';
// WP-F (P2-c): reusable file-change evidence UI. `TaskChangesList` is pure
// presentation (any row source); `TaskChangesPanel` adds the per-task fetch.
export { TaskChangesList, TaskChangesPanel } from './TaskChangesPanel';
export { TaskDoneBurst } from './TaskDoneBurst';
export { TASK_DONE_XP, celebrateTaskDone } from './task-celebrate';
