/**
 * UI parser for run transcripts (paperclip adapter-ui-parser contract 1.0.0).
 *
 * Constraints per the contract: zero runtime imports, no Node/DOM APIs, no
 * module-level side effects, deterministic, never throws.
 */

export type TranscriptEntry =
  | {
      kind: "assistant" | "thinking" | "user" | "stderr" | "system" | "stdout";
      ts: string;
      text: string;
    }
  | { kind: "tool_call"; ts: string; name: string; input: unknown; toolUseId?: string }
  | { kind: "tool_result"; ts: string; toolUseId: string; content: string; isError: boolean };

/**
 * Stateless line parser. The DuDuClaw adapter logs three shapes on stdout:
 * - `[duduclaw] dispatched to agent '<id>' (message_id=...)` → system
 * - `--- Response N (<ts>) ---` headers from check_responses → system
 * - everything else → assistant text (the agent's reply)
 */
export function parseStdoutLine(line: string, ts: string): TranscriptEntry[] {
  try {
    const trimmed = line ?? "";
    if (trimmed.trim().length === 0) {
      return [];
    }
    if (trimmed.startsWith("[duduclaw]")) {
      return [{ kind: "system", ts, text: trimmed }];
    }
    if (/^--- Response \d+ \(/.test(trimmed) || /^Found \d+ response\(s\)/.test(trimmed)) {
      return [{ kind: "system", ts, text: trimmed }];
    }
    return [{ kind: "assistant", ts, text: trimmed }];
  } catch {
    return [{ kind: "stdout", ts, text: String(line) }];
  }
}
