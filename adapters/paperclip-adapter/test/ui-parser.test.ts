import { describe, expect, it } from "vitest";
import { parseStdoutLine } from "../src/ui-parser.js";

describe("parseStdoutLine", () => {
  it("classifies adapter markers as system entries", () => {
    const out = parseStdoutLine("[duduclaw] dispatched to agent 'boss' (message_id=x)", "t1");
    expect(out).toEqual([
      { kind: "system", ts: "t1", text: "[duduclaw] dispatched to agent 'boss' (message_id=x)" },
    ]);
    expect(parseStdoutLine("--- Response 1 (2026-07-11T00:00:00Z) ---", "t2")[0]?.kind).toBe(
      "system",
    );
    expect(parseStdoutLine("Found 1 response(s) from agent 'boss':", "t3")[0]?.kind).toBe(
      "system",
    );
  });

  it("treats agent text as assistant output and skips blank lines", () => {
    expect(parseStdoutLine("Hello there", "t")).toEqual([
      { kind: "assistant", ts: "t", text: "Hello there" },
    ]);
    expect(parseStdoutLine("   ", "t")).toEqual([]);
  });
});
