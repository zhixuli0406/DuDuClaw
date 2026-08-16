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
// I-2b: reusable deliverable UI. `TaskArtifactsList` is pure presentation (the
// coming 成品畫廊 feeds it a different row source); `TaskArtifactsPanel` adds
// the per-task fetch.
export { TaskArtifactsList, TaskArtifactsPanel, artifactIcon } from './TaskArtifactsPanel';
// I-2a: the 「檔案」tab — same `attachments/` provenance data as `/files`
// (I-2b), filtered to one task. `TaskFilesList` is pure presentation.
export { TaskFilesList, TaskFilesPanel, type TaskFileRow } from './TaskFilesPanel';
export { TaskDoneBurst } from './TaskDoneBurst';
export { TASK_DONE_XP, celebrateTaskDone } from './task-celebrate';
// I-2c: goal-loop detail sections merged from the retired `/goals?task=`
// dialog into `/tasks/:id` — contract cards, pending kickoff, round
// timeline, pause-reason chip, and the goal-mode-only takeover action.
export {
  PauseReasonChip,
  GoalTakeoverButton,
  GoalContractCards,
  GoalPendingKickoff,
  GoalRoundTimeline,
  GoalRoundBadge,
  GoalDiminishingBadge,
  GoalDeadlineBadge,
} from './GoalLoopPanel';
// I-3b: pin/rename/archive — not goal-specific, but currently only consumed
// by `/goals`.
export { TaskListActionsMenu, RenameTaskDialog } from './TaskListActionsMenu';
