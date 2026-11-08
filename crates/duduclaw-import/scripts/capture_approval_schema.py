"""Capture only reviewed ApprovalStore initializer SQL; never opens legacy data.

Writes approvals-v180.sql: the current source is the v1.80.x store.
approvals-v1793.sql is a separate pinned catalog of the older release and is
never regenerated from current source."""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[3]
GATEWAY = ROOT / "crates/duduclaw-gateway/src/approval"
OUTPUT = ROOT / "crates/duduclaw-import/src/approvals-v180.sql"
HEADER = (
    "-- Fresh current (v1.80.x line) ApprovalStore schema; generated from reviewed initializers by\n"
    "-- scripts/capture_approval_schema.py. approvals-v1793.sql is the pinned older catalog and is never\n"
    "-- regenerated. Difference: RM-027 decision lease columns approvals.lease_token / lease_until.\n"
)


def blocks(path):
    # Initializers only: test fixtures after #[cfg(test)] are not schema.
    text = path.read_text().split("#[cfg(test)]", 1)[0]
    return re.findall(r'execute_batch\(\s*"((?:\\.|[^"\\])*)"', text, re.S)


def sql_literal(raw):
    # Reviewed SQL literals contain no Unicode or Rust continuation escapes.
    return raw.replace(r'\"', '"').replace(r'\n', '\n').replace(r'\\', '\\')


store = blocks(GATEWAY / "store.rs")
operations = blocks(GATEWAY / "operations.rs")
grants = blocks(GATEWAY / "workflow_grants/migration.rs")
assert len(store) == 5 and store[0].startswith("PRAGMA ") and store[4].strip() == "DETACH DATABASE quarantine_live_events"
assert len(operations) == 2 and operations[1].strip().startswith("UPDATE approvals SET")
assert len(grants) == 2
store = store[:4]
operations = operations[:1]
columns = [
    "notify_channel TEXT", "notify_chat_id TEXT", "reminded_at TEXT", "simulation TEXT",
    "request_kind TEXT NOT NULL DEFAULT 'approval'", "binding_json TEXT", "answer_json TEXT",
    "invalidated_reason TEXT", "on_behalf_of TEXT", "origin_session_id TEXT", "origin_turn_id TEXT",
    "lease_token TEXT", "lease_until TEXT",
]
initializer = (GATEWAY / "store.rs").read_text()
for ddl in columns:
    assert f'"{ddl}"' in initializer, ddl
authority_columns = [
    "authority_source TEXT NOT NULL DEFAULT 'bound_human_approval'", "revision_grant_id TEXT",
    "revision_grant_epoch INTEGER", "revision_grant_hash TEXT", "run_authority_json TEXT",
]
parts = [sql_literal(store[0]), sql_literal(store[1])]
parts += [f"ALTER TABLE approvals ADD COLUMN {ddl};" for ddl in columns]
parts += [sql_literal(store[2]), "ALTER TABLE cancelled_approval_turns ADD COLUMN principal TEXT;", sql_literal(store[3])]
parts += [sql_literal(operations[0]), sql_literal(grants[0])]
parts += [f"ALTER TABLE main.approval_operations ADD COLUMN {ddl};" for ddl in authority_columns]
parts += [sql_literal(grants[1]), "INSERT INTO approval_authority_schema VALUES(1);"]
OUTPUT.write_text(HEADER + "\n\n".join(parts) + "\n")
