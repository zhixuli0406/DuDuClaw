# 自主目標迴圈（Goal Loop）

在對話裡丟一個目標，AI 員工就會自主規劃、執行、自我驗收，做到完成或卡住時回來通知你。這一頁說明從頻道使用 `/goal` 的方式、自主程度（AutonomyLevel）分級、相關設定鍵，以及卡住轉人工時的按鈕語意。

整套預設關閉，不影響現有的一問一答對話。啟用需要在 `config.toml` 設定 `[dispatch] enabled = true`（詳見下方設定）。

---

## `/goal` 指令

在任何已接通的頻道（Telegram / Discord / Slack / LINE / …）對 AI 員工輸入：

| 指令 | 行為 |
|------|------|
| `/goal <目標描述>` | 建立一個自主目標任務，指派給當前對話的 AI 員工。沒有另外指定驗收標準時，以目標描述本身當作驗收基準。 |
| `/goal <目標> \|\| <驗收標準>` | 用 `\|\|` 分隔：前半是目標，後半是驗收標準（判官核可的依據）。 |
| `/goal status` | 列出當前 AI 員工進行中的目標任務（短碼 / 狀態 / 第幾輪）。 |
| `/goal` | 顯示用法說明。 |

**範例**

```
/goal 整理這批客戶資料成月報並寄出 || 報表含每月營收圖表，寄到 boss@example.com
```

建立後會回覆確認訊息，包含任務短碼、上限輪數，以及「完成或卡住會在這裡通知你」。任務進度與需人工的通知會**推回你發起的這個對話**（來源頻道），而不只是 AI 員工的 `[proactive]` 通知頻道。

> 若尚未啟用自主派工引擎（`[dispatch] enabled = true`），任務仍會建立，但確認訊息會提醒你它不會自動開始執行。

---

## 外層進度看板

目標任務的每個狀態轉移都會推一則簡短（一到三行）的進度訊息回來源對話：

- 開始執行 / 重試（第 N/上限 輪）
- 驗收中
- 未通過 → 修正後重試（附驗收判官回饋摘要）
- 完成 ✅（附結果摘要）
- 卡住 → 需要你決定（同時另外推送審批按鈕）

同一任務同一狀態不會重複推播。來源對話不存在時，退回 AI 員工的 `[proactive]` 頻道；兩者都沒有時只寫入儀表板 Activity Feed，不打擾你。

---

## AutonomyLevel 自主程度五級

每個 AI 員工的自主程度由 `agent.toml [capabilities] autonomy_level` 一個刻度控制。未設定 / 無法解析 → 預設 **Approver**（保守：只有卡住或需人工才問你）。

| 級別 | 行為 |
|------|------|
| `operator` | 迴圈完全不自主驅動；任務建立後靜置，由人手動推進。 |
| `collaborator` | 第一次派工前需人工核准（kickoff 審批），核准後自主重試到完成。 |
| `consultant` | 同 collaborator 的 kickoff 審批。 |
| `approver` | **預設**。無 kickoff 閘；卡住 / 需人工時才轉人工審批。 |
| `observer` | 全自動；需人工時只通知、不等待（任務自動結束）。 |

```toml
# agent.toml
[capabilities]
autonomy_level = "approver"
```

---

## 設定鍵

### `config.toml`（全域）

```toml
[dispatch]
enabled = true          # 啟用自主派工引擎（含 goal loop 驅動器）。預設 false

[goal_loop]
iteration_cap = 8       # 每個任務的硬性派工上限，超過 → 轉人工。預設 8
wall_clock_hours = 24   # 從建立起算的牆鐘預算（小時），超過 → 轉人工。預設 24
max_concurrent = 3      # 同時在飛的目標任務上限（防 spawn 風暴）。預設 3
tick_secs = 30          # 驅動器輪詢週期（秒）。預設 30
stalled_secs = 600      # 派工後未被認領視為停滯、可重派的秒數。預設 600

[dispatch_guard]        # 回饋路徑斷路器（防再生型無限迴圈）
window_secs = 60        # 滑動窗長度（秒）。預設 60
max_in_window = 20      # 一個窗內允許的派工次數，超過即熔斷。預設 20
cooldown_secs = 60      # 熔斷後拒絕派工的冷卻秒數。預設 60
max_hop_depth = 5       # 委派鏈跨行程 re-spawn 的深度上限。預設 5
```

所有區塊都可省略；缺省 / 部分設定一律退回上表的內建預設。

### `agent.toml`（每個 AI 員工）

```toml
[capabilities]
autonomy_level = "approver"
irreversible_tools = ["send_email"]          # 一律需人工核准的不可逆工具
maybe_irreversible_tools = ["Bash", "http_post"]  # 由 judge 判定是否需升人
```

---

## needs_human 按鈕語意

任務轉「需人工」時（達派工上限 / 牆鐘超時 / 連續兩輪駁回且回饋雷同 / 驗收判官在重試預算耗盡時仍不通過），會推送三顆按鈕到 AI 員工的控制頻道：

| 按鈕 | 動作 |
|------|------|
| 重試 | 任務回到待重試（`pending`），下一輪驅動器再派工。 |
| 標記完成 | 直接標記完成（`done`）。 |
| 放棄 | 取消任務（`cancelled`）。 |

按鈕決策是**冪等且失敗關閉**的：只會從 `needs_human` 狀態轉出，重複按或狀態已變一律無效（no-op）。`collaborator` / `consultant` 的 kickoff 審批同理，逾時未決＝拒絕（fail-closed）。

---

## 終止保證

驅動器（而非模型）掌握硬邊界，卡住的目標不可能無限迴圈：完成訊號只認驗收判官核可（不信任 AI 自評「做完了」）；派工上限、牆鐘上限、並行上限、進度震盪偵測、回饋路徑斷路器各自獨立生效，任何一條踩線即轉人工或熔斷。
