# RFC-22: Multi-Agent Coordination Design Principles

> **Status**: Approved (2026-05-05)
> **Decided by**: lizhixu (project owner)
> **Discussion trigger**: 2026-05-05 端到端 Discord trace 揭露 4 個結構性問題
> **Related**: [RFC-21 Identity Resolution](RFC-21-identity-credential-isolation.md), [TODO-gateway-revival-202605.md](../commercial/docs/TODO-gateway-revival-202605.md)

---

## Background

2026-05-05 14:13–14:36 UTC，使用者透過 Discord 發送「請 DuDuClaw TL/PM 透過共用 wiki 討論下一個可實作功能」。系統執行了 23 分鐘的多 agent 協調，最終 wiki 寫出 16.7KB 三段內容，agnes 也回覆了 Discord 1698 字。但 `tool_calls.jsonl` 揭露的真相：

```
14:22:19 spawn_agent duduclaw-tl ❌ FAIL (first attempt)
14:22:38 spawn_agent duduclaw-tl ✅ SUCCESS (retry 1)
14:22:46 spawn_agent duduclaw-pm ❌ FAIL (only 1 attempt, gave up)
14:26:15 spawn_agent duduclaw-tl ✅ SUCCESS (retry 2 — to write PM section!)
```

**Wiki 中「DuDuClaw PM 的觀點」段落是 agnes/TL 在 PM spawn 失敗後 hallucinate 補寫的**，並聲稱「透過 TL-DuDuClaw 委派機制回覆」。這是 multi-agent 系統的嚴重信任問題。

同時暴露 3 個結構性決策從未被明確選邊：
- Task vs Wiki 哪個是協作 substrate
- 同步 spawn vs 非同步 bus_queue
- Channel 訊息該不該 fan-out 到 sub-agent

本 RFC 確立四個決策原則作為後續修復與設計的依據。

---

## Decision 1: Task = Execution Unit, Wiki = Knowledge Base (Two-tier)

**選項 C**（兩層架構）

### Principle

- **Task Board**（`tasks.db`）= 結構化執行單元，狀態機（todo / in_progress / blocked / done），有 owner、priority、deadline
- **Shared Wiki**（`shared/wiki/`）= 內容知識庫，討論、決策、設計、報告
- **每次重大協調**：root agent 必須 **同時** 建 task + 寫 wiki，task.description 引用 wiki path

### Implementation Constraints

1. agnes SOUL.md 加入「重大協調時必先 `tasks_create`」
2. Wiki frontmatter 引用對應 task ID（新增 `task_ids: [<uuid>, ...]` 欄位）
3. `shared_wiki_write` MCP tool 在 description 提示 caller：「若此次寫入是執行單元，建議同時建 task」（軟提示，不強制）
4. autopilot Rule A（unassigned task → delegate to agnes triage）保持，作為 task-first 的驗證閉環

### Rationale

純 Wiki-first 會失去執行追蹤；純 Task-first 會失去討論脈絡。Two-tier 將「狀態」與「內容」解耦，task board 作為調度入口，wiki 作為知識基礎。autopilot rules 自然有 fan-in（從 task event 觸發），sub-agent 心跳消費 task 即可參與協作。

---

## Decision 2: Hybrid Spawn + Bus Fallback

**選項 C**（同步快路 + 非同步可靠路）

### Principle

`spawn_agent` MCP tool 內部實作如下流程：

```
1. 嘗試同步 spawn（fork claude CLI 子進程，等回應）
2. spawn 成功 → 正常返回
3. spawn 失敗 → 自動 fallback 到 send_message:
   a. 寫 bus_queue.jsonl（target sub-agent）
   b. 觸發 events.db AgentMessageQueued event
   c. 對 caller 返回 partial result: {
        "spawn_status": "deferred",
        "delivery": "bus_queue",
        "expected_pickup_within_minutes": 60,
        "task_id": <linked task if any>
      }
4. caller (agnes) 收到 deferred 後，必須：
   - 在 reply 中告訴使用者「<sub-agent> 將於 ~10 分鐘內處理（async fallback）」
   - 不得 hallucinate 該 sub-agent 的觀點（見 Decision 4）
```

### Implementation Constraints

1. `spawn_agent` 工具的 description 明確標註 fallback 行為
2. `send_message` 寫入 bus_queue 時 attach `linked_task_id` 與 `delivery_priority`
3. Sub-agent heartbeat 消費 bus_queue 時，必須在 reply 中標明「by async fallback」
4. 增加 metric：`spawn_to_bus_fallback_count`（per agent / per day）

### Rationale

純 sync spawn 失敗會 hallucinate 或放棄；純 async bus 等心跳要 1h，使用者體驗差。Hybrid 給 fast path 但保留訊息（不會丟），失敗時透明告知使用者，避免 5/5 那次 PM 段落造假。

---

## Decision 3: Channel Mapping — Threads/Channels Bind to Specific Agents

**選項 D**（每個 thread/channel 綁定特定 agent）

### Principle

`agent.toml` 擴充：

```toml
[channels.discord]
bot_token = "..."

# 新增：thread/channel binding
[[channels.discord.bindings]]
type = "thread"  # or "channel" or "guild"
id = "1501225251910979704"
description = "W22 Sprint Planning"

[[channels.discord.bindings]]
type = "channel"
id = "1495730722156318901"
description = "general chat"
```

### Routing Logic

```
Channel message arrives → look up bindings:
  1. 若有 thread_id 綁定 specific agent → 直接 dispatch 到該 agent
  2. 若有 channel_id 綁定 → dispatch 到該 agent
  3. 若有 guild_id 綁定 → dispatch 到該 agent
  4. fallback → root agent (agnes)

Prediction engine user_id 一律保留為原始 channel user ID
（不再強制 root agent 收 Discord 訊息）
```

### Implementation Constraints

1. `agent.toml` schema 擴充 + migration（既有 agent 預設無 binding，行為不變）
2. `channel_reply.rs` 路由邏輯改成「先查 binding，再 fallback root」
3. Dashboard 加 UI：「為這個 thread 綁定哪個 agent」
4. Prediction `user_id` 一律用真實 channel user ID（非 `agent:_bus`），讓 sub-agent 也能累積真實 channel feedback

### Rationale

最簡單實作（純 config，無新 condition op）；user_id 保真讓 prediction engine 自然分散；sub-agent 可直接從 channel 拿訊號（解決「14 天只 1 個 SOUL 變動」覆蓋失衡）；root agent 退化為 fallback router 角色，職責更清楚。

優於 mention-routing（B）：不需要 LLM 解析 `@mention`；優於 broadcast event（C）：不需要新增 condition op；優於 root-only（A）：不會結構性失衡。

---

## Decision 4: Hallucination Forbidden + Audit Trail

**選項 D**（嚴格禁止代寫 + audit log）

### Principle

#### 4.1 嚴格禁止代寫（Behavior Contract）

每個 agent 的 `CONTRACT.toml`（或同等）加入：

```toml
[must_not]
forbid_proxy_authoring = """
不得代理其他 agent 撰寫意見、觀點、報告。
若 spawn_agent 失敗，wiki 應標記「[此處空缺：<target_agent> 不可達 (timestamp)]」，
並在 reply 中明確告知使用者該 agent unreachable，
不得編造該 agent 的回應。
"""

[must_always]
mark_unreachable_agents = """
當 sub-agent spawn 失敗或 bus_queue fallback 時，
產出的內容（wiki / task / channel reply）必須標記：
  - 「pending: <agent_id>」for 已 enqueue 但未回覆
  - 「unreachable: <agent_id>」for 完全失敗
"""
```

#### 4.2 Wiki Schema 擴充：actually_written_by + claimed_authors

`shared_wiki_write` MCP tool 寫入時自動寫 frontmatter：

```yaml
---
title: "..."
actually_written_by: agnes      # MCP caller agent_id (從 tool_calls audit 來)
claimed_authors: [agnes, duduclaw-tl, duduclaw-pm]  # 文中聲稱的作者
pending_authors: [duduclaw-pm]   # 因 spawn fail 未實際回覆的 agent
trust_level: 0.6                 # 若 pending_authors 非空，trust 自動降權
---
```

#### 4.3 tool_calls.jsonl Audit 增強

`shared_wiki_write` 與 `shared_wiki_append` 的 audit log 加入：

```json
{
  "tool_name": "shared_wiki_write",
  "agent_id": "agnes",
  "params_summary": "path=pm/w22-discussion.md size=16743",
  "claimed_authors_in_content": ["agnes", "duduclaw-tl", "duduclaw-pm"],
  "actual_caller": "agnes",
  "matches_caller": false,        // 新增：true 若所有 claimed 都包含 actual
  "success": true,
  "timestamp": "..."
}
```

Dashboard 顯示「文中聲稱多 agent 共寫但實際只有單 agent caller」的紅旗。

### Implementation Constraints

1. `crates/duduclaw-core/src/contract.rs` 解析 `[must_not]` / `[must_always]` 並注入 system prompt
2. `shared_wiki_write` MCP tool 偵測 markdown 中 `## <agent> 的觀點` 模式，提取 claimed authors
3. `tool_calls.jsonl` schema 加 `claimed_authors_in_content`、`actual_caller`、`matches_caller`
4. Wiki frontmatter 加 `pending_authors`、`actually_written_by`
5. Dashboard 加「Wiki Authorship Audit」面板

### Rationale

5/5 wiki 中 PM 段落造假是 multi-agent 系統的核心信任問題。純禁止（A）使用者沒法檢查；純標記（B）不夠強制；結構強制（C）太僵硬。Two-stage（A + audit）兼顧：行為層禁止（透過 SOUL/CONTRACT）+ 系統層追溯（透過 audit log + frontmatter），即使 LLM 違規 hallucinate 也能在 audit 中被偵測並降權。

---

## Implementation Order (Linked to TODO)

| Bug | Decision | 修法摘要 |
|---|---|---|
| **P1-7** (channel_reply 漏 cost flush) | 獨立 | `channel_reply.rs` 在 stream-json 結束後補 `cost_telemetry.record()` |
| **P1-8** (sub-agent cost 漏寫) | Decision 2-C, 4-D | 真因不是漏 flush，是 PM spawn 失敗 hallucinate；修 spawn fallback + behavior contract |
| **P1-9a** (agnes 不用 task board) | Decision 1-C | agnes SOUL.md 加「重大協調必先 tasks_create」 |
| **P1-9b** (autopilot condition 失效) | 獨立 | `lookup_path("task.assigned_to")` 修 bug — 應正確解析 nested field |
| **P1-9c** (delegate action 沒 enqueue) | 獨立 | `action_delegate` → `enqueue_prompt` 沒寫 bus_queue，補錯誤處理 + log |
| **P1-10** (sub-agent MCP api_key) | 獨立 | runner.rs spawn 時用該 sub-agent 的 API key + namespace，不繼承父進程 env |
| **P0-3b** (sub-agent channel coverage) | Decision 3-D | `agent.toml [[channels.discord.bindings]]` + `channel_reply.rs` 路由邏輯 |

---

## Migration Plan

### Phase 1（本次 sprint，立即）
- 修 P1-9b/9c（autopilot 兩個獨立 bug）
- 修 P1-7（channel_reply cost flush，獨立）
- 修 P1-10（sub-agent MCP key，獨立）

### Phase 2（下一個 sprint）
- 實作 Decision 4: Behavior Contract + Wiki Audit（最重要，先做）
- 實作 Decision 2: Spawn → Bus Fallback
- 修 P1-8 + P1-9a（依賴 Phase 2 完成）

### Phase 3（W22-W23）
- 實作 Decision 3: Channel Bindings
- 修 P0-3b
- Dashboard Audit 面板

### Phase 4（長期）
- 實作 Decision 1 完整版：task ↔ wiki 互相引用 schema
- agent SOUL must_not / must_always 自動 system prompt 注入
- Two-tier coordination 全面驗證

---

## Open Questions

1. **Q**: `CONTRACT.toml` 已存在還是新檔？
   - 從 commercial/CLAUDE.md 提到 `CONTRACT.toml with must_not / must_always boundaries + duduclaw test red-team CLI`，似乎已存在
   - **TODO**: Phase 2 實作前先確認位置

2. **Q**: `actually_written_by` 是否影響既有 wiki 索引（`_wiki.db`）？
   - 需 schema migration
   - **TODO**: Phase 2 實作前評估

3. **Q**: Channel Bindings 的 admin UI 該怎麼設計？
   - Dashboard 還是 CLI（`duduclaw channels bind --thread <id> --agent <name>`）？
   - **TODO**: Phase 3 開始前討論

---

## Reviewer Notes

此 RFC 由 5/5 真實事件驅動，非紙上談兵。所有「現況」均有 log / db trace 為證據（見 [TODO-gateway-revival-202605.md](../commercial/docs/TODO-gateway-revival-202605.md) 的「端到端驗證」章節）。

決策原則一旦寫入 RFC，後續所有 multi-agent 相關 PR 都需檢查是否符合本 RFC 規範。違反時應在 PR description 中明確說明 trade-off。

---

*Approved by lizhixu | 2026-05-05*
*Implementation owner: TBD per phase*
