-- v1.85 RM-182: exact-session references; schema recognized, never imported as authority.
CREATE TABLE session_tool_refs_v2 (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL, session_generation TEXT NOT NULL, created_at INTEGER NOT NULL,
        tool_name TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_id TEXT NOT NULL, label TEXT NOT NULL,
        CONSTRAINT rm182_ref_limits_v1 CHECK (
    length(session_id) BETWEEN 1 AND 512 AND instr(session_id, char(0)) = 0
    AND length(tool_name) BETWEEN 1 AND 64 AND instr(tool_name, char(0)) = 0
    AND length(ref_kind) BETWEEN 1 AND 32 AND instr(ref_kind, char(0)) = 0
    AND length(ref_id) BETWEEN 1 AND 256 AND instr(ref_id, char(0)) = 0
    AND ref_id NOT GLOB '*[^A-Za-z0-9_.:/-]*'
    AND length(label) <= 120 AND instr(label, char(0)) = 0
    AND ((tool_name = 'gmail_create_draft' AND ref_kind = 'gmail_draft')
      OR (tool_name = 'calendar_create_event' AND ref_kind = 'calendar_event')
      OR (tool_name = 'docs_create' AND ref_kind = 'google_doc')
      OR (tool_name = 'sheets_create' AND ref_kind = 'google_sheet')
      OR (tool_name IN ('tasks_create', 'gtasks_create') AND ref_kind = 'task')))
    );
ALTER TABLE session_tool_refs_v2 RENAME TO session_tool_refs;
        CREATE INDEX idx_session_tool_refs_session ON session_tool_refs(session_id, id);
