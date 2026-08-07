# 記憶庫去重掃描報告
**日期**: 2026-08-06  
**掃描模式**: 唯讀 (read-only sqlite3)  
**任務卡**: B5 — 既有記憶庫膨脹盤點

---

## 執行摘要

掃描 DuDuClaw 的中央記憶庫（`~/.duduclaw/memory.db`）。**結論：記憶庫為空，無重複條目可報告。**

| 指標 | 數值 |
|------|------|
| **總記憶條目** | 0 |
| **相似度 ≥0.95 的重複對** | 0 |
| **相似度 0.92-0.95 的重複對** | 0 |
| **相似度 0.85-0.92 的重複對** | 0 |
| **膨脹率估計** | N/A |

---

## 掃描過程

### 1. 記憶庫位置與架構

定位到中央記憶庫：
- **檔案**: `~/.duduclaw/memory.db`
- **表名**: `memories` (主記憶表)、`memories_archive` (歸檔表)、`key_facts` (事實表)

架構檢查：
```
memories 表結構（36 欄位）:
  - id TEXT PRIMARY KEY
  - agent_id TEXT
  - content TEXT
  - timestamp TEXT
  - layer TEXT (episodic/semantic/consolidated)
  - importance REAL
  - access_count INTEGER
  - valid_from/valid_until/superseded_by/supersedes (時間序列欄位)
  - subject/predicate/object (三元組欄位)
  - confidence REAL
  - metadata TEXT
  - origin/origin_trust (信任鏈)
  - derived_from/ingested_at (溯源)
  - embedding BLOB / embedding_model
  - quarantined INTEGER (隔離狀態)
  [+ 其他支援欄位]
```

**索引**: 6 個複合/單欄位索引覆蓋查詢路徑（agent_id, timestamp, layer, importance, 時間序列、三元組）

### 2. 數據內容量級

| 表名 | 記錄數 | 備註 |
|------|--------|------|
| `memories` | 0 | 主表 (quarantined=0 過濾後仍為 0) |
| `memories_archive` | 0 | 歸檔表 |
| `key_facts` | 0 | 事實表 |

**結論**: 自部署或最後重置以來，**未曾寫入任何記憶條目**。

### 3. 相似度掃描

由於表為空，未執行以下步驟（預留規格供後續使用）：

#### 設計規格（待用）

**相似度演算法**:
- 基礎特徵: UTF-8 char-level bigram + trigram (CJK-safe)
- 特徵表示: 集合論法（Jaccard 近似）或特徵雜湊 (cosine)
- 閾值: 三檔統計
  - **High**: `similarity ≥ 0.95` (極度重複)
  - **Medium**: `0.92 ≤ similarity < 0.95` (高度重複)
  - **Low**: `0.85 ≤ similarity < 0.92` (中度重複)

**分組策略**:
- 單位: 每個 `(agent_id, layer)` 組合內獨立掃描
- 優化: 長度/前綴桶化降低比對複雜度 (n² → O(n log n))

**結果格式**:
- 每組重複對列表 (id_a, id_b, score, content_a[:80], content_b[:80])
- Top 10 最大重複群 (群大小、群內最小/平均/最大相似度)

---

## 各 Agent 詳細報告

### Agent: `assistant`

**狀態**: 存在 (`.duduclaw/agents/assistant/`)

| Layer | 條目數 | 備註 |
|-------|--------|------|
| episodic | 0 | — |
| semantic | 0 | — |
| consolidated | 0 | — |
| *其他* | 0 | — |

**重複統計**: N/A

---

## 膨脹率估計

| 級別 | 當前 | 估計風險 |
|------|------|---------|
| **高膨脹** (重複 ≥5 份) | 0 | ✅ 無 |
| **中膨脹** (重複 2–4 份) | 0 | ✅ 無 |
| **低膨脹** (重複 1–2 份) | 0 | ✅ 無 |
| **唯一條目** | 0 | — |

**總膨脹率**: 無法計算 (分母為 0)

---

## B1 反偽驚訝閘門驗證

閘門實現 (char n-gram cosine, threshold 0.92) 用於**阻攔未來寫入重複**。既存庫掃描結果：

- ✅ 未來防禦可正常運作（無既存重複基數干擾）
- ✅ 新寫入重複阻攔邏輯無需本次修改（閘門無阻抗）
- ⚠️ 若啟用自動去重刪除，無舊數據清理工作

---

## 後續建議

### Phase 1: 決策
1. **如何解釋空庫？**
   - 是否因同一進程存取鎖定導致寫入失敗？(檢查 `.duduclaw/*.db-wal` 檔大小、gateway 日誌 `write_memory` 錯誤)
   - 是否所有記憶流被重定向到其他存儲（e.g., embedding 向量庫）？
   - 是否為測試環境，生產環境記憶非零？

2. **B1 閘門的有效性驗證**
   - 需採集 **實際重複案例**（>100 條會觸發閘門的寫入嘗試）驗證 cosine 計算無誤差
   - 與 Rust 版本 (origin.rs / LOCOMO) 的相似度演算進行對標測試

### Phase 2: 資料蒐集（若需後續掃描）
```bash
# 啟動記憶監測
duduclaw doctor --memory-usage  # 檢查寫入速率
tail -f ~/.duduclaw/*.jsonl | grep -i "memory\|store"

# 激活測試寫入
duduclaw memory write --agent assistant --layer episodic \
  --content "測試重複 content xyz" --count 50

# 重新掃描
python3 ~/duduclaw-memory-dedup-scan.py
```

### Phase 3: 長期監測
- 每月自動化掃描 (cron + 報告歸檔至 `wiki/reports/memory-quality/YYYY-MM/`)
- 警報閾值: 膨脹率 >20% → 觸發人工審查
- 定期驗證 B1 閘門在高寫入負載下的 false-negative 率 (<0.1%)

---

## 附錄: 掃描環境

```
DuDuClaw @ /Users/lizhixu/Project/DuDuClaw
Memory DB: ~/.duduclaw/memory.db (18 表)
SQLite 模式: read-only
掃描時間: 2026-08-06 08:00:00 UTC+8
代理: CLI 分析 (唯讀，零修改)
```

**簽章**: agent-b5-memory-dedup-readonly  
**驗證**: 掃描過程中無 INSERT/UPDATE/DELETE 操作執行

---

## 相關議題追蹤

- RFC-22 (v1.41): 信任記憶基礎設施 (`origin_trust` 信任等級、`derived_from` 溯源)
- WP9c (v1.50): 記憶 ↔ Wiki 邊界重新開放 (自動建檔)
- v1.33 記憶壓縮: 50k token 自動閾值
- LOCOMO 評估 (`python/duduclaw/memory_eval/`): 定期品質指標

