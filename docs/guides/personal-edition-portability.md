# 個人版資料可攜：自架 ↔ 代管互轉

> 適用：DuDuClaw 個人版（Personal Edition）。代管的個人版實例與你自架的個人版是**同一份產物**，
> 因此資料可在兩者間自由搬移，沒有 vendor lock-in。

## 為什麼可攜

個人版（`EditionProfile::Personal`）是一個**自足、單一擁有者**的部署單位。Cloud「代管」只是
「幫你在我們的基礎設施上跑同一份個人版」——容器內帶的就是 `DUDUCLAW_EDITION=personal`。
所以你的全部狀態都集中在一個目錄 `~/.duduclaw/`：

| 內容 | 路徑 |
|------|------|
| Agents（SOUL.md / CLAUDE.md / agent.toml / .claude） | `~/.duduclaw/agents/` |
| 記憶（episodic / semantic SQLite + FTS5） | `~/.duduclaw/memory*.sqlite` |
| 設定 | `~/.duduclaw/config.toml`、`~/.duduclaw/inference.toml` |
| 授權 | `~/.duduclaw/license.json` |
| 任務 / 自動化 / 事件 | `~/.duduclaw/*.jsonl`、`events.db` |

## 立即可用：手動搬移（tar）

今天就能用標準工具搬移整份個人版狀態：

```bash
# 1. 從來源（自架或代管匯出的目錄）打包
tar -C "$HOME" -czf duduclaw-export.tar.gz .duduclaw

# 2. 搬到目標機器後解開（先停掉 gateway）
tar -C "$HOME" -xzf duduclaw-export.tar.gz

# 3. 啟動，個人版會直接載入既有 agents / 記憶
duduclaw start
```

> 代管客戶可在 Dashboard 申請匯出，取得同格式的 `~/.duduclaw/` tarball，解開後即可自架——
> 反之亦然。因為兩端是同一份個人版產物，**不需要任何轉換**。

## 切換時的注意事項

- **授權**：`license.json` 綁定機器指紋（hostname + MAC）。換機器後個人版核心照常運作（Apache 2.0），
  若有 Pro 加值模組，依 [spec-license-module.md](../../commercial/docs/spec-license-module.md) §7.3 走 self-serve 重新綁定。
- **頻道 token**：channel bot token 在加密設定內，會一起搬過去；換 IP/網域時記得更新 webhook URL。
- **EditionProfile**：自架預設 `personal`；可用 `DUDUCLAW_EDITION` 環境變數或 `agent.toml [edition] profile`
  覆寫（優先序見 [personal-edition-plan.md](../../commercial/docs/personal-edition-plan.md) §4）。

## 路線圖（規劃中）

- Dashboard「一鍵匯出我的資料」按鈕（產生 tarball）。
- 啟動時一鍵匯入代管匯出的 tarball。
- 代管 ↔ 自架 round-trip 一致性自動驗證。

追蹤：`commercial/docs/TODO-personal-edition.md` P4。
