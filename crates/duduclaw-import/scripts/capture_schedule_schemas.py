"""Extract reviewed initializers from source, never from legacy databases."""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "crates/duduclaw-import/src"


def batches(relative, start, end):
    source = (ROOT / relative).read_text().split(start, 1)[1].split(end, 1)[0]
    return re.findall(r'execute_batch\(\s*"((?:\\.|[^"\\])*)"', source, re.S)


autopilot = batches("crates/duduclaw-gateway/src/autopilot_store.rs",
                    "fn init_schema(conn:", "// ── Rules CRUD")
assert len(autopilot) == 1
autopilot += ["ALTER TABLE autopilot_rules ADD COLUMN sequence TEXT;",
              "ALTER TABLE autopilot_rules ADD COLUMN metadata TEXT;"]
workflow = batches("crates/duduclaw-gateway/src/workflow/store.rs",
                   "fn initialize(connection:", "super::draft_store::init_schema")
assert len(workflow) == 3
drafts = batches("crates/duduclaw-gateway/src/workflow/draft_store.rs",
                 "pub fn init_schema(tx:", "let version:")
assert len(drafts) == 1
for name, blocks in [("autopilot", autopilot), ("workflow", workflow + drafts)]:
    (OUT / f"{name}-v1793.sql").write_text(
        f"-- Exact reviewed v1.79.3 {name} initializer.\n" + "\n\n".join(blocks) + "\n")
