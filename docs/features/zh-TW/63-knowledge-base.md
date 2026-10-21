# 文件知識庫

把公司文件（員工手冊、報價單、產品常見問題）放進**集合**，每個集合各自決定誰能讀，AI 員工就能搜尋這些文件，並指出答案出自哪一頁、哪張投影片或工作表的哪幾列。

這一版包含：儲存、匯入、關鍵字搜尋、`knowledge.*` RPC、上傳路由、三支唯讀 MCP 工具，以及 `duduclaw knowledge eval` 指令。儀表板頁面、向量（語意）搜尋、通道回覆時自動帶入段落與引用頁尾，會在之後的版本提供。既有的 wiki 完全不受影響。

---

## 集合與權限

權限以集合為單位。每一筆授權寫明授權對象與角色：

| 授權對象 | 識別 | 可給的角色 | 涵蓋誰 |
|---|---|---|---|
| user | 儀表板使用者 id | 檢視者、編輯者、擁有者 | 這個人 |
| department | 部門名稱（完全相同才算） | 檢視者、編輯者 | 帳號屬於這個部門的人，以及 `[agent] department` 設成這個部門的 AI 員工 |
| agent | AI 員工 id | 檢視者 | 這位 AI 員工 |
| channel | `line`（整個通道）或 `line:<聊天室 id>` | 檢視者 | 在這個通道或聊天室講話的**任何人**，包括沒有儀表板帳號的人 |

- **檢視者**可以搜尋、讀段落、下載原檔、看來源清單。**編輯者**另外可以新增、刪除、重建來源。**擁有者**另外可以改權限、改設定、刪除集合。
- 管理員等於所有集合的擁有者。主管可以建立集合，建立後成為擁有者；一般員工不能建立集合。
- 新集合只有建立者看得到。讀不到或格式壞掉的授權一律不算數。每個集合至少要保留一位使用者擁有者。
- `channel` 授權適合公開資料（例如常見問題）：任何加官方帳號好友的人都查得到。

### 一次搜尋能讀哪些集合

儀表板使用者：自己有任何角色的集合（管理員：全部）。

AI 員工替某人回答時，取下列條件的交集：

1. **員工這一側**：授權給這位員工或它所屬部門的集合，再依 `agent.toml` 縮小：

   ```toml
   [knowledge]
   collections = ["hr-handbook", "product-faq"]   # 集合代號；[] 代表關閉知識庫工具
   ```

   沒有 `[knowledge]` 段落代表所有已授權給它的集合。不認得的代號會被忽略。gateway 解析不了這個段落時當作 `[]`。員工不能自己修改這個段落。

2. **人這一側**：這個 turn 在替誰回答。已登入的使用者帶入自己與部門的授權；身分不明的通訊軟體發話者只帶入 `channel` 授權；沒有人的 turn（排程、心跳、派工）可以用授權給員工、敏感度為 public 或 internal 的集合。

3. **敏感度**：集合分 `public`、`internal`（預設）、`personal`、`restricted`。不是一對一的對話（群組，以及所有無法證明是私聊的情況）會排除 `personal` 與 `restricted`。

4. **縮小範圍**：`knowledge_search` 的 `collections` 參數只能讓範圍變小。

這一版 gateway 還不知道 WebChat 或通道的 turn 是哪位已登入的人（這要等觸發詞路由那一部分上線）。在那之前，正在進行的通道或 WebChat turn 裡的工具呼叫，一律當作該聊天室裡身分不明的發話者（只有 `channel` 授權）；沒有進行中的 turn 時當作沒有人的 turn。這比最終行為更窄，不會更寬。

## 加入文件

| 方式 | 誰可以 | 說明 |
|---|---|---|
| `POST /api/knowledge/upload` | 編輯者 | multipart：先放 `collection_id` 欄位，再放 1 到 20 個 `file`；使用儀表板使用者的 Bearer token |
| `knowledge.sources.add` `{kind: "paste"}` | 編輯者 | 貼上的文字，存成 `.txt` 來源 |
| `knowledge.sources.add` `{kind: "url"}` | 編輯者 | 公開的 `http`／`https` 網頁，處理時才抓取，不使用快取 |

支援格式：有文字層的 PDF、Word `.docx`、Excel `.xlsx`／`.xls`、PowerPoint `.pptx`、CSV、純文字、Markdown；網址來源支援網頁。舊版 `.doc`／`.ppt`、有密碼的檔案、圖片與音訊會被拒收，訊息會說明怎麼轉檔。掃描版 PDF 沒有文字層會被拒收（目前沒有 OCR）；可讀的 PDF 裡沒有文字的頁面會列在警告中。

同一份檔案重複上傳到同一個集合會回「已存在」並傳回原本的來源。不同集合可以放同一份檔案。

來源狀態依序是 `queued → extracting → indexing → ready`。還沒有向量模型時，`ready` 會帶 `status_detail = "keyword_only"`。失敗時會留下固定的代碼與給上傳者看的說明：

| 代碼 | 意思 |
|---|---|
| `too_large`、`too_many_chars` | 超過 `max_source_bytes` 或 `max_source_chars`；整份拒收，不會截斷 |
| `quota_sources`、`quota_chunks`、`quota_collections` | 達到設定的上限 |
| `unsupported_format`、`legacy_office`、`encrypted`、`no_text_layer`、`empty_text`、`encoding_unknown` | 檔案無法讀成文字 |
| `document_limits` | Office 檔沒通過壓縮炸彈與巢狀深度檢查 |
| `extract_timeout`、`extract_crashed`、`extract_oom` | 解析程序逾時、當掉或記憶體不足 |
| `url_blocked`、`url_fetch_failed`、`url_too_large` | 網址是內部位址、連不上、不是 HTML 或純文字，或超過 5 MB |
| `retry_exhausted` | 處理途中 gateway 中斷三次 |

重建失敗時，舊的索引仍可搜尋，錯誤會顯示在旁邊。

### 檔案怎麼被讀取

解析一律不在 gateway 裡做。每個檔案由子程序 `duduclaw knowledge-extract`（同一支執行檔、空的環境變數）讀取：

- 牆鐘時間上限（`extract_timeout_secs`，預設 180 秒），超過就結束整個程序群組；
- macOS 與 Linux 有 CPU 時間上限、不產生 core dump，Linux 另有位址空間上限（`extract_memory_mb`，預設 2048）；
- macOS（Seatbelt）與 Linux（Landlock）用作業系統沙箱，禁止任何寫入與網路；
- Office 檔先通過既有的壓縮炸彈與 XML 深度檢查；
- 輸出超過 `max_source_chars × 4` bytes 就中止並拒收。

Windows 沒有沙箱也沒有記憶體上限，只有時間上限；這類來源會帶 `extract_unsandboxed` 警告。同一時間只跑一個解析。

文字先做 NFKC 正規化（全形英數字與標點轉成半形），再切成約 500 字、彼此略有重疊的段落：PDF 不跨頁（頁尾太短的殘段併到下一頁的第一段），工作表只在列與列之間切開，每一段都附上表頭列。每一段都會檢查是否像提示注入的文字，命中的段落會被標記並顯示警告。

## 搜尋

這一版只有關鍵字搜尋：

- 三個字以上的詞走 SQLite FTS5 trigram 索引。中文查詢會在「的、了、嗎」這類助詞處斷開，所以「請假的規定」也找得到「請假規定」。
- 兩個字的詞（「請假」、`HR`）走有上限的子字串掃描，最多 2 萬段；掃描到上限時結果會標 `scan_capped`。
- trigram 找到不到三段時，較長詞語拆出的兩字片段也會加入子字串掃描。
- 幾份清單以 reciprocal rank fusion 合併，每份來源最多三段。

每次結果都會說明檢索模式，以及向量搜尋為什麼沒啟用。和文件用詞不同的問法（用「薪水」問「薪資」）要等向量搜尋上線才找得到。

## AI 員工怎麼用

三支唯讀 MCP 工具，scope 是 `knowledge:read`（不能授權給外部 MCP 金鑰）：

| 工具 | 參數 | 回傳 |
|---|---|---|
| `knowledge_search` | `query`、`collections`（集合代號，只能縮小）、`limit`（1–20，預設 5） | `[n] 標題 · 位置（集合） chunk_id=…` 加段落內文，並標明是文件內容 |
| `knowledge_get_passage` | `chunk_id`、`context`（0–2） | 該段落與前後文 |
| `knowledge_list_collections` | — | 這個 turn 可讀的集合、來源數與段落數 |

只要知識庫啟用、員工的 `[knowledge]` 沒有關閉，就會列出這三支工具。沒有任何授權的員工也看得到，呼叫時會被告知沒有可讀的集合。工具透過 gateway 的簽章本機路由查詢；gateway 沒在執行時會回報原因，絕不直接讀資料庫。

## 稽核

- 每次搜尋、讀段落、列清單、下載原檔都寫一筆到 `knowledge.db` 的存取紀錄：誰（儀表板使用者、員工、通道）、查詢內容（前 500 字）、可讀的集合、結果的 id，不存段落內文。超過 `access_log_retention_days` 的紀錄每小時清理一次。
- 建立、修改、刪除集合，修改權限，刪除來源，同時寫進 `security_audit.jsonl`。
- 集合擁有者可以用 `knowledge.access_log.list` 查看存取紀錄。

## 設定（`config.toml`）

```toml
[knowledge]
enabled = true
max_source_bytes = 104857600     # 單檔 100 MB；可設 1 MB–200 MB（改上傳上限需重新啟動）
max_source_chars = 500000
max_sources_per_collection = 300
max_collections = 200
max_chunks_total = 200000
extract_timeout_secs = 180
extract_memory_mb = 2048         # 只在 Linux 生效
access_log_retention_days = 180
```

超出範圍的值會被調回邊界。`enabled = false` 時工具不列出，所有 RPC 與上傳都會被拒絕。

## 檢查檢索品質

```
duduclaw knowledge eval --docs <資料夾> --questions <題目.jsonl> [--k 5] [--min-recall 0.85] [--report out.json]
```

在暫存資料夾建一個一次性的資料庫，用和 gateway 相同的解析程序與索引方式匯入資料夾裡的文件，再回報各題型的 Recall@1／5／10、MRR@10、無解題得到空結果的比例，以及查詢延遲。Recall@k 低於 `--min-recall` 時結束碼為 1。題目每行長這樣：

```json
{"id":"q01","question":"到職滿一年的特休有幾天？","type":"factoid","gold":[{"source":"員工手冊.md","contains":"特別休假七日"}]}
{"id":"q13","question":"公司有員工宿舍嗎？","type":"unanswerable","answerable":false}
```

gold 也可以寫 `page`、`slide`、`sheet`、`row`。repo 裡的 `crates/duduclaw-knowledge/tests/fixtures/eval/` 有一份虛構的小題組；客戶的真實文件與題目請不要放進 repo。

## 資料與刪除

所有資料都在 `<home>/knowledge/`：`knowledge.db`，以及 `blobs/` 底下以 SHA-256 命名的原檔。刪除來源時，段落、索引與向量在同一個交易內刪除，沒有其他集合使用的原檔也一併刪除；資料庫開啟 `secure_delete`。刪除集合必須輸入集合名稱確認。

## 相關文件

- [MCP 工具](../../guides/zh-TW/mcp-tools.md)
- [安全防護](05-security-defense.md)
- [儀表板設定](../../guides/zh-TW/dashboard-settings.md)
