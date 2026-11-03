// Hide task entry points for every role; set false to restore the existing UI.
export const TASK_ENTRY_POINTS_HIDDEN = true;

export function isTaskEntryVisible(path: string): boolean {
  return !TASK_ENTRY_POINTS_HIDDEN || !['/tasks', '/goals', '/assign'].includes(path);
}

export function chatEntryLabel(previous: string): string {
  return TASK_ENTRY_POINTS_HIDDEN ? 'nav.newChat' : previous;
}
