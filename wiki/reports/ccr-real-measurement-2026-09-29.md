# CCR 自家高輸出工具實測（X1 方案 3）— 2026-09-29

> 狀態：**未量測（SKIPPED — 本機無可用 provider 憑證）**。
> `config.toml [ccr] enabled` 維持 **false**；本次只把「哪些工具可以被 CCR 接住」補齊（`builtin_sources`），
> 不動開關。
> 相關程式：`crates/duduclaw-gateway/src/ccr_runtime.rs`、`crates/duduclaw-cli/src/ccr_run_cmd.rs`、
> 規格：`docs/spec/reversible-context-ccr.md`。

## 1. CCR 目前真正覆蓋哪些執行路徑

盤點 `CcrRuntime` 的實際建構點與消費點（`grep` 實錘，非推論）：

| 路徑 | 進入點 | CCR 是否生效 |
|---|---|---|
| 通道回覆（十一通道） | `channel_reply.rs:10266` → `ccr_runtime::for_agent` | **是**（需 `[ccr] enabled = true` 且 allowlist 命中） |
| 派工／cron／heartbeat／goal-loop | `claude_runner.rs:1642`、`claude_runner.rs:1922` | **是**（同上） |
| 地端推論 tool loop | `local_llm.rs:471` | **是**（同上） |
| `duduclaw-llm::tool_loop`（openai-compat／direct API） | `tool_loop.rs` 的 `ccr: Option<&CcrRuntime>` 參數 | **是**，但只在上面三個呼叫端有傳入時 |
| Claude CLI spawn 的內建工具（`Read`／`Bash`／`WebFetch`） | — | **否**。CLI 自帶工具不經 MCP，`ToolInterceptor`／CCR 都攔不到 |
| codex／gemini／antigravity runtime | — | **否**（與 redaction 的 MCP proxy 同一個已知缺口） |
| PTY session pool | — | **否**（pooled REPL 活過 per-call spawn，需 session-owned guard） |

另外兩個硬前提，本次未改：
- `for_agent` 要求有 **具名 principal**（`CHANNEL_REPLY_USER_ID`）與非 `mcp-session` 的 session id；
  沒有兩者就整個關閉（fail-closed），所以 utility turn／系統呼叫本來就吃不到 CCR。
- `allowed_sources` 為空 ⇒ 拒絕所有來源；`enabled = false` ⇒ allowlist 被清空 ⇒ `for_agent` 回 `None`。

## 2. 本次改了什麼

`[ccr] builtin_sources`（**預設 true**）把 DuDuClaw 自家 MCP server 的 11 個高輸出唯讀工具
併進 `allowed_sources`：

```
db_select · db_query · csv_read · xlsx_read · file_read
web_fetch_cached · web_extract
shared_wiki_read · wiki_read
memory_search · memory_fetch_batch
```

（工具名以 `crates/duduclaw-cli/src/mcp.rs` 的實際註冊名為準 —— 是 `web_fetch_cached`，不是 `web_fetch`。）

行為邊界，已用測試焊死：
- `enabled = false`（預設）⇒ allowlist 仍為空、`for_agent` 仍回 `None`。**內建來源不會自己把 CCR 打開。**
- `builtin_sources = false` ⇒ 與改動前逐位相同（`effective_allowed_sources` 原樣回傳操作者清單）。
- `min_compress_bytes` 維持 4096 ⇒ 小於 4 KiB 的工具結果一律不壓縮，逐位不變。
- 清單只收唯讀、可回取的工具；有一條不變量測試擋掉 `SELF_ECHO_TOOL_NAMES` 與 write/create/update/delete/send/spawn 字樣。

## 3. 為什麼沒有真量測

`duduclaw ccr-compare-run` 需要一個真 provider（`ccr_run_cmd.rs` 走 `providers::build_provider`
＋`resolve_env_key`）。本機檢查結果：

| 檢查 | 結果 |
|---|---|
| `env \| grep -c '_API_KEY='` | `0` |
| `~/.duduclaw/config.toml` 的 `[accounts]` 區段 | **不存在** |
| `~/.duduclaw/` 下的帳號儲存檔 | 無 |

沒有憑證就沒有四臂（Raw／Lossless／Lossy／CCR）的真實 token 與 quality 數字。
**不編造、不用合成數字冒充實測**——這正是規格自己要求的誠實邊界。

## 4. 之後要量測時怎麼做（一行開法 + 判準）

```bash
# 1) 準備憑證（任一 provider 的 API key 即可，走 duduclaw accounts 或環境變數）
# 2) 真量測（32 個任務上限，evidence 落在自己指定的私有檔）
duduclaw ccr-compare-run \
  --tasks fixtures/ccr/native-replay-synthetic.json \
  --evidence-out /tmp/ccr_evidence.json
```

無憑證時可先跑不需 provider 的機制回歸：`duduclaw ccr-compare-synthetic`
（合成 provider，驗的是四臂管線本身，**不是**生產節省幅度）。

判準（依 `ccr_replay.rs` 的 `Observation`）：對同一組任務，CCR 臂相對 Raw 臂
① `input_tokens + cache_read_tokens + output_tokens` 的淨值下降，
② `success`（`oracle_exact` 逐字比對）不下降。
兩者同時成立才把 `[ccr] enabled` 預設改 true，並在 `CHANGELOG` 的 `### Changed` 明講行為變更；
否則維持 false，數字寫回本檔。

開啟方式（達標後，或操作者自行評估）：

```toml
[ccr]
enabled = true          # 這一行就是全部
# builtin_sources = true  # 已是預設，不必寫
```
