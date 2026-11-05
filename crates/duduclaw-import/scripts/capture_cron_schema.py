"""Extract the reviewed v1.79.3 CronStore initializer, never legacy bytes."""
from pathlib import Path
import re
ROOT = Path(__file__).resolve().parents[3]
source = (ROOT / "crates/duduclaw-gateway/src/cron_store.rs").read_text()
initializer = source.split("    fn init_schema(conn: &Connection)", 1)[1].split("    // ── Reads", 1)[0]
blocks = re.findall(r'execute_batch\(\s*"((?:\\.|[^"\\])*)"', initializer, re.S)
assert len(blocks) == 2
columns = ["notify_channel", "notify_chat_id", "notify_thread_id", "cron_timezone", "trigger_kind", "condition_script", "condition_state", "watch_command", "on_behalf_of"]
for column in columns:
    assert f'"{column}"' in initializer
sql = "\n\n".join(blocks) + "\n" + "\n".join(f"ALTER TABLE cron_tasks ADD COLUMN {column} TEXT;" for column in columns)
(ROOT / "crates/duduclaw-import/src/cron-v1793.sql").write_text("-- Exact CronStore initializer and ordered additive columns, v1.79.3.\n" + sql + "\n")
