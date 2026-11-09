#!/usr/bin/env python3
"""Reversible users.db tampering for the disposable acceptance host (matrix items 2 and 5).

Run as the service account (the harness uses setpriv) against the DISPOSABLE
service home only. Each action records what it changed in --save and `restore`
puts everything back, so a later item starts from a clean fixture.

  force-bind    drop trigger isolation_personal_binding_insert and insert a second
                user's binding on a Personal employee (what a database-level
                attacker, or a bug that bypassed the A3 triggers, would do)
  replace-tenant overwrite auth_meta 'isolation_tenant_id' with a new random UUIDv4
  restore       undo every recorded change

Prints one JSON line {"ok": bool, "detail": str}. Schema facts used (duduclaw-auth
db.rs / isolation.rs): user_agent_bindings(user_id, agent_name, access_level,
bound_at); trigger isolation_personal_binding_insert; auth_meta(key, value).
"""
import argparse
import json
import sqlite3
import sys
import uuid

TRIGGER = "isolation_personal_binding_insert"
TENANT_KEY = "isolation_tenant_id"


def connect(path):
    conn = sqlite3.connect(path, timeout=15, isolation_level=None)
    conn.execute("PRAGMA busy_timeout=15000")
    return conn


def load(path):
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        return {"undo": []}


def store(path, record):
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(record, handle)


def force_bind(conn, args, record):
    row = conn.execute("SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?", (TRIGGER,)).fetchone()
    if row is None:
        raise RuntimeError(f"trigger {TRIGGER} not found (schema changed?)")
    conn.execute("BEGIN IMMEDIATE")
    try:
        conn.execute(f"DROP TRIGGER {TRIGGER}")
        conn.execute("INSERT INTO user_agent_bindings(user_id, agent_name, access_level, bound_at) "
                     "VALUES(?, ?, 'viewer', 'acceptance')", (args.user, args.agent))
        conn.execute("COMMIT")
    except Exception:
        conn.execute("ROLLBACK")
        raise
    record["undo"].append({"kind": "unbind", "user": args.user, "agent": args.agent})
    record["undo"].append({"kind": "sql", "sql": row[0]})
    return f"bound {args.user} to {args.agent} with {TRIGGER} dropped"


def replace_tenant(conn, _args, record):
    original = conn.execute("SELECT value FROM auth_meta WHERE key=?", (TENANT_KEY,)).fetchone()
    if original is None:
        raise RuntimeError("isolation tenant row not found")
    triggers = [r[0] for r in conn.execute(
        "SELECT sql FROM sqlite_master WHERE type='trigger' AND tbl_name='auth_meta' AND sql IS NOT NULL")]
    names = [r[0] for r in conn.execute(
        "SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='auth_meta'")]
    conn.execute("BEGIN IMMEDIATE")
    try:
        for name in names:
            conn.execute(f'DROP TRIGGER "{name}"')
        conn.execute("UPDATE auth_meta SET value=? WHERE key=?", (str(uuid.uuid4()), TENANT_KEY))
        conn.execute("COMMIT")
    except Exception:
        conn.execute("ROLLBACK")
        raise
    record["undo"].append({"kind": "tenant", "value": original[0]})
    for sql in triggers:
        record["undo"].append({"kind": "sql", "sql": sql})
    return "installation tenant replaced"


def restore(conn, _args, record):
    conn.execute("BEGIN IMMEDIATE")
    try:
        for step in reversed(record["undo"]):
            if step["kind"] == "unbind":
                conn.execute("DELETE FROM user_agent_bindings WHERE user_id=? AND agent_name=?",
                             (step["user"], step["agent"]))
            elif step["kind"] == "tenant":
                conn.execute("UPDATE auth_meta SET value=? WHERE key=?", (step["value"], TENANT_KEY))
            elif step["kind"] == "sql":
                conn.execute(step["sql"])
        conn.execute("COMMIT")
    except Exception:
        conn.execute("ROLLBACK")
        raise
    record["undo"] = []
    return "restored"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=["force-bind", "replace-tenant", "restore"])
    parser.add_argument("--db", required=True)
    parser.add_argument("--save", required=True)
    parser.add_argument("--user")
    parser.add_argument("--agent")
    args = parser.parse_args()
    record = load(args.save)
    try:
        conn = connect(args.db)
        if args.action == "force-bind" and not (args.user and args.agent):
            raise RuntimeError("--user and --agent are required")
        detail = {"force-bind": force_bind, "replace-tenant": replace_tenant, "restore": restore}[args.action](
            conn, args, record)
        store(args.save, record)
        print(json.dumps({"ok": True, "detail": detail}))
    except (sqlite3.Error, RuntimeError) as error:
        store(args.save, record)
        print(json.dumps({"ok": False, "detail": str(error)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
