# Where thinking comes from

How each runtime reports an AI employee's thinking to the [thinking display](64-thinking-display.md). Every report goes through the same per-turn gate (masking, truncation, CONTRACT and guardrail checks) before anyone sees it, and nothing here changes how much a model thinks or what a turn costs: `[model] effort` stays the only cost setting.

A runtime only changes its request when the person reading the turn may see thinking text (a signed-in WebChat user with display `summary`). Cron jobs, task dispatch, heartbeats and turns whose viewer only sees the timer send exactly the same command line or request body as before.

| Runtime | What is shown | When the request changes |
|---|---|---|
| Claude CLI | Live thinking text (when the CLI returns it), a running token estimate, redacted blocks, and the turn's thinking-token total | `--include-partial-messages` is added only for a viewer who may read the text |
| Codex CLI | Reasoning summaries (`reasoning` items) and reasoning tokens, after the run | `-c model_reasoning_summary=auto` only for a viewer who may read the text |
| OpenAI-compatible APIs (DeepSeek, Qwen, vLLM, OpenRouter, …) | Raw reasoning (`reasoning_content`, `reasoning`, or a leading `<think>…</think>` block); shown only with redaction on | Never for the plain and tool-loop calls without a text viewer; with one, the tool loop streams so reasoning appears as it is written |
| Anthropic Direct API | Thinking summaries, omitted and redacted blocks, after the call | A summarized `thinking` display is requested only for a text viewer and only on models that think on every request anyway (Opus 5 / 5.5, Sonnet 5 / 5.5, Haiku 5.5, Fable, Mythos), so the cost does not change |
| Antigravity (`agy`) | Nothing yet (timer only) | Never |
| Grok CLI | Nothing (timer only) | Never |

## Details

- **Claude CLI.** New models often return thinking blocks with empty text in `-p` mode; those show as "this model did not provide a thinking summary". `system` / `thinking_tokens` frames (newer CLIs) give a token estimate even then. Partial-message lines are consumed by the thinking parser and never reach the task tree, foresight, the operator result capture or the answer text, so an answer is never counted twice. Signatures and redacted thinking data never leave the CLI parser. With `DUDUCLAW_STREAM_DEBUG=1`, `claude_stream.log` records each `thinking_delta` line as `{"type":"stream_event","elided":"thinking_delta"}` instead of its text; partial-message lines are also kept out of failure diagnostics.
- **Codex CLI.** Reasoning items are read after the run finishes (the CLI output is not streamed line by line yet), so the summary appears when the answer does.
- **OpenAI-compatible APIs.** Open models return their own reasoning, so the text is only shown when redaction is enabled; otherwise the viewer sees seconds and tokens with a hint. A `<think>` tag is recognised only at the very start of the answer (after whitespace); a block without a closing tag counts as reasoning to the end. The answer the employee sends is unchanged: the leading `<think>` block is removed from it as before. The tool loop's messages, including reasoning parts, are passed back to the model exactly as the provider returned them. A provider that refuses streaming gets a plain call instead, so the turn still completes; a streamed answer without a usage chunk is billed from a token estimate and marked `usage_estimated = 1` in `cost_telemetry.db`.
- **Anthropic Direct API and the operator tool loop.** Summaries are reported after each call. Signatures and `redacted_thinking` data are passed back to the model unchanged and are never shown.
- **Antigravity.** Step types other than `tool` are only written to the debug log by name, so they can be mapped once their vocabulary is known.

Not verified with real quota yet: whether `claude -p` returns non-empty thinking text on current models, how `codex exec` emits reasoning items on a real account, and agy's step vocabulary. The parsers were tested against fixtures and a stub CLI.
