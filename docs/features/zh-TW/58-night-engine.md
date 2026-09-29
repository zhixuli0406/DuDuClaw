# 夜間引擎 — 趁 AI 員工閒著的時候整理

一個整天在跟人講話的 AI 員工，記憶會愈來愈亂：同一件事被用五種說法重複寫進去、一套流程分散在三段對話裡學到一半、還有沒人回頭處理的待辦問題。夜間引擎就是在閒置時段做這些整理的背景工作，順便為隔天先讀一點資料。

它**預設關閉，而且有兩道各自獨立的開關**。這篇文件存在的理由很實際：在此之前，維運者沒有任何管道知道要開哪裡。

---

## 它實際上做什麼

一次「夜間 pass」對一個 agent 跑四個子階段：

| 階段 | 名稱 | 需要 LLM？ | 產出 |
|---|---|---|---|
| N1 | 睡眠期運算 | 是 | 對 agent 現有脈絡先做推理，結果快取到下次喚醒時用 |
| N2 | 主動預取 | 是 | 依歷史＋記憶預測下一個需求，提前把佐證材料收集好 |
| N3 | Schema 歸納 | 否 | 把情節記憶裡反覆出現的模式提升成 schema 條目 |
| N4 | 複現門檻式整併 | 否 | 只有語意上反覆出現的知識才觸發整併，寫入前還要通過涵蓋度／保存度／忠實度檢查，不過就回滾 |

N3、N4 是確定性的，零成本。N1、N2 打的是該 agent 的**輔助（utility）模型**（預設 haiku 級），也只有這兩個會受花費上限影響。

論文依據：睡眠期運算（arXiv:2504.13171）、ProAct（arXiv:2605.25971）、DCPM schema 歸納（arXiv:2606.09483）、RecMem（arXiv:2605.16045）＋ TRUSTMEM（arXiv:2606.25161）。

---

## 怎麼開

兩道開關都要開。這是刻意的：全域那道是維運者的預算決定，per-agent 那道是「哪些員工適用」的決定。

**1. 全域 — `~/.duduclaw/config.toml`：**

```toml
[night]
# 允許 N1/N2 這兩個吃 LLM 的子階段真的去呼叫模型。
# 缺這個鍵、寫 false、或型別寫錯，一律視為關閉；這個旋鈕只能往「開」的方向撥。
llm_enabled = true
```

`llm_enabled = false`（預設）時排程器照常跑，但 N1/N2 拿不到模型，會留一行紀錄後跳過。N3、N4 不受影響，它們本來就不需要模型。

**2. 每個 AI 員工 — `~/.duduclaw/agents/<id>/agent.toml`：**

```toml
[night_engine]
enabled = true                  # 這個 agent 的總開關，預設 false
idle_threshold_minutes = 90     # 多久沒有使用者互動就算閒置
max_pass_cost_cents = 20        # 每次 pass 的硬性花費上限，碰到就停掉 N1/N2
max_passes_per_day = 8          # 斷路器，每個 agent 滾動 24 小時內的上限

sleep_time = true               # N1
prefetch = true                 # N2
schema_induction = true         # N3（確定性）
recurrence_consolidation = true # N4（確定性）

schema_min_support = 3          # N3：一個模式要出現幾次才升格成 schema
recurrence_threshold = 3        # N4：語意複現幾次才觸發整併
context_window = 40             # N1/N2：每次 pass 看多少則近期記憶／回合
```

這個段落**只**從該 agent 自己的 `agent.toml` 讀取。`config.toml` 沒有全域 `[night_engine]` 後備——registry 載入每個 agent 的設定時不會疊上任何全域預設值，所以沒寫 `[night_engine]` 的 agent 拿到的就是上面那組內建預設（`enabled = false`）。要替多位員工開啟，就在各自的 `agent.toml` 裡分別寫 `enabled = true`。

---

## 成本與安全

- **每 pass 花費上限。** `max_pass_cost_cents` 在每次 N1/N2 呼叫前檢查、呼叫後記錄估算成本。撞到上限就跳過剩下的 LLM 子階段，N3/N4 繼續跑。
- **每日斷路器。** `max_passes_per_day` 限制單一 agent 滾動 24 小時內的 pass 次數，狀態會持久化，所以重啟或當掉重跑都不會把計數洗掉。斷路器檔案損毀時以全新狀態啟動（fail-safe）。
- **只在閒置時動。** 要該 agent 超過 `idle_threshold_minutes` 沒有使用者互動才會觸發；從來沒活動過的 agent 也算閒置。
- **記憶內容一律當資料。** 兩個夜間 prompt 都把記憶片段包在 `<data>` 區塊裡，並明確指示不得執行裡面出現的任何指令——記憶列來自通道，攻擊者有辦法寫進去。
- **寫入走既有的閘門。** N3/N4 的整併寫入使用共用的記憶引擎建構點，所以 `[memory] novelty_gate` 在這裡跟在其他 gateway 內部寫入路徑上一樣生效。

---

## 怎麼確認它真的跑了

夜間 pass 的日誌來自 `duduclaw_gateway::night_engine` 這個 tracing target。跑完會有 `night pass complete` 並列出實際執行的子階段；被跳過會說原因（例如 `night pass skipped: daily circuit breaker open`），而 agent 根本不閒置時則完全不出聲。

```bash
# systemd
journalctl -u duduclaw -f | grep night

# 前景執行
RUST_LOG=duduclaw_gateway::night_engine=debug duduclaw run
```

如果什麼都沒有：確認該 agent 真的閒置、兩道開關都開了、而且當天的 `max_passes_per_day` 還沒用完。

---

## 延伸閱讀

- [記憶與知識庫](../../guides/zh-TW/memory-and-knowledge.md) — 記憶裡放什麼、怎麼被取回
- [演化開關](../../guides/zh-TW/evolution-switches.md) — 其他背景自我改進迴圈與它們的停止開關
