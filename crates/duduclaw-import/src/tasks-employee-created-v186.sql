-- v1.86 employee task listing index; no new imported authority.
CREATE INDEX IF NOT EXISTS idx_tasks_employee_created ON tasks(assigned_to, created_at DESC, id ASC) WHERE archived = 0;
