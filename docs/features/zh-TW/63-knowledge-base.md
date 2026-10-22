# 文件知識庫

把公司文件（員工手冊、報價單、產品常見問題）放進**集合**，每個集合各自決定誰能讀，AI 員工就能搜尋這些文件，並指出答案出自哪一頁、哪張投影片或工作表的哪幾列。

目前包含：儲存、匯入、關鍵字搜尋、`knowledge.*` RPC、上傳路由、三支唯讀 MCP 工具、`duduclaw knowledge eval` 指令，以及儀表板的文件知識庫頁面與 AI 員工綁定設定。向量（語意）搜尋、通道回覆時自動帶入段落與引用頁尾，會在之後的版本提供。既有的 wiki 完全不受影響。

---

## 在儀表板操作

側邊欄「公司」群組裡的**文件知識庫**（個人版在主選單，排在「記憶」後面）列出你看得到的集合：名稱、敏感度、你的角色、文件數與處理中的數量。

1. **建立集合**：主管以上按「新增集合」，填名稱、說明、敏感度，代號可留空自動產生（AI 員工設定用的就是這個代號）。建立後只有你看得到。
2. **加入來源**：在集合的「來源」分頁按「新增來源」，可以拖放或選擇檔案（一次最多 20 個；格式不支援或超過單檔上限的檔案在送出前就會被擋下並說明，會顯示上傳進度，結束後逐檔列出已加入、已存在或拒收原因）、貼上文字，或加入公開網頁。每份文件旁的標籤顯示目前狀態：排隊中、讀取內容中、建立索引中、完成（字詞比對）、等待審批、失敗（附原因）。還在處理的文件，清單會每 3 秒自動更新。每份文件的選單可以下載原始檔、重新讀取、刪除（後兩項需要編輯者）。
3. **試搜尋**：輸入問題，看這個集合依序回傳哪些段落、各在第幾頁或哪個章節；點結果會在側邊抽屜打開原文，段落本身會標示出來，可以往前或往後一段。
4. **權限**（只有擁有者能看與修改）：右邊的選擇器分「同事、部門、AI 員工、通道」，只列出你能分享的對象：企業版才有同事與部門，同事要輸入至少 2 個字搜尋（最多列 20 位，不會整份列出）；AI 員工只列出你擁有的員工（管理員可選全部，只能檢視或操作的員工不算）；通道可以選整個通道，或填聊天室 ID 只限某個聊天室。角色選單會停用該類對象不能擁有的角色，最後一位擁有者不能移除。分享給整個通道時會出現警告。按「儲存分享設定」一次存檔；若期間有人改過，會請你重新載入。gateway 存檔時用同一套規則再檢查一次：新增的對象，以及角色被調高的對象（例如檢視者改成編輯者），不在你能分享的範圍內就會被拒絕；角色不變或調低的既有對象不受影響。
5. **存取紀錄**（擁有者）：時間、誰（同事、AI 員工代哪個人、哪個通道上未綁定帳號的人）、做了什麼、問了什麼、拿到幾段。
6. **設定**（擁有者）：名稱、說明、部門標籤、敏感度、「只在這台主機上處理」（之後的語意搜尋不會把這個集合送到外部服務），以及輸入名稱確認後刪除整個集合。

管理員在文件知識庫頁右上角另有「被拒絕的讀取」，列出所有集合裡被擋下的讀取。

**讓 AI 員工使用集合**：先在集合的權限頁把集合分享給這位 AI 員工或它的部門，再到「AI 員工 → 編輯設定 → 工具與權限 → 文件知識庫」選擇：使用所有分享給它的集合、只用勾選的集合，或不使用。這裡寫入的就是 `agent.toml` 的 `[knowledge] collections`；只能勾選已分享給它的集合，所以這個設定只會縮小範圍。能改的人是這位員工的擁有者與管理員。

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

   沒有 `[knowledge]` 段落代表所有已授權給它的集合。不認得的代號會被忽略。gateway 解析不了這個段落時當作 `[]`。員工不能自己修改這個段落；它的擁有者與管理員可以在儀表板設定（見上方「讓 AI 員工使用集合」）。

2. **人這一側**：這個 turn 在替誰回答。已登入的使用者帶入自己與部門的授權；身分不明的通訊軟體發話者只帶入 `channel` 授權；gateway 登記為沒有人的 turn（排程、心跳、派工）可以用授權給員工、敏感度為 public 或 internal 的集合；gateway 沒有登記的呼叫（沒有 turn id 或對不上的 id）什麼都讀不到。

3. **敏感度**：集合分 `public`、`internal`（預設）、`personal`、`restricted`。不是一對一的對話（群組，以及所有無法證明是私聊的情況）會排除 `personal` 與 `restricted`。

4. **縮小範圍**：`knowledge_search` 的 `collections` 參數只能讓範圍變小。

這一版 gateway 還不知道 WebChat 或通道的 turn 是哪位已登入的人（這要等觸發詞路由那一部分上線），排程、心跳、派工的 turn 也還沒有登記。在那之前，正在進行的通道或 WebChat turn 裡的工具呼叫（不論哪一種 runtime）一律當作該聊天室裡身分不明的發話者（只有 `channel` 授權），其他呼叫什麼都讀不到。這比最終行為更窄，不會更寬。

## 加入文件

| 方式 | 誰可以 | 說明 |
|---|---|---|
| `POST /api/knowledge/upload` | 編輯者 | multipart：先放 `collection_id` 欄位，再放 1 到 20 個 `file`；使用儀表板使用者的 Bearer token |
| `knowledge.sources.add` `{kind: "paste"}` | 編輯者 | 貼上的文字，存成 `.txt` 來源 |
| `knowledge.sources.add` `{kind: "url"}` | 編輯者 | 公開的 `http`／`https` 網頁，處理時才抓取，不使用快取；依網頁宣告的編碼（Big5、GBK 等）解碼；網址會先正規化（去掉 `#` 片段、網域轉小寫、去掉結尾 `/`），同一頁不會重複加入 |

支援格式：有文字層的 PDF、Word `.docx`、Excel `.xlsx`／`.xls`、PowerPoint `.pptx`、CSV、純文字、Markdown；網址來源支援網頁。舊版 `.doc`／`.ppt`、有密碼的檔案、圖片與音訊會被拒收，訊息會說明怎麼轉檔。掃描版 PDF 沒有文字層會被拒收（目前沒有 OCR）；可讀的 PDF 裡沒有文字的頁面會列在警告中。

同一份檔案重複上傳到同一個集合會回「已存在」並傳回原本的來源。不同集合可以放同一份檔案。

來源狀態依序是 `queued → extracting → indexing → ready`。還沒有向量模型時，`ready` 會帶 `status_detail = "keyword_only"`。失敗時會留下固定的代碼與給上傳者看的說明：

| 代碼 | 意思 |
|---|---|
| `too_large`、`too_many_chars` | 超過 `max_source_bytes` 或 `max_source_chars`；整份拒收，不會截斷 |
| `quota_sources`、`quota_chunks`、`quota_collections`、`quota_bytes` | 達到設定的上限 |
| `unsupported_format`、`legacy_office`、`encrypted`、`no_text_layer`、`empty_text`、`encoding_unknown` | 檔案無法讀成文字 |
| `document_limits` | Office 檔沒通過壓縮炸彈與巢狀深度檢查 |
| `extract_timeout`、`extract_crashed`、`extract_oom` | 解析程序逾時、當掉或記憶體不足 |
| `url_blocked`、`url_fetch_failed`、`url_too_large` | 網址是內部位址、連不上、不是 HTML 或純文字，或超過 5 MB |
| `sandbox_unavailable` | 這台主機無法使用解析用的作業系統沙箱（見下方） |
| `retry_exhausted` | 處理途中 gateway 中斷三次 |

重建失敗時，舊的索引仍可搜尋，錯誤會顯示在旁邊。

### 檔案怎麼被讀取

解析一律不在 gateway 裡做。每個檔案由子程序 `duduclaw knowledge-extract`（同一支執行檔、空的環境變數）讀取：

- 牆鐘時間上限（`extract_timeout_secs`，預設 180 秒），超過就結束整個程序群組；
- macOS 與 Linux 有 CPU 時間上限、不產生 core dump，Linux 另有位址空間上限（`extract_memory_mb`，預設 2048）；
- macOS（Seatbelt）與 Linux（Landlock）用作業系統沙箱，禁止任何寫入與網路，讀取只限該檔案本身、解析程式與系統函式庫目錄；
- Office 檔先通過既有的壓縮炸彈與 XML 深度檢查；
- 輸出超過 `max_source_chars × 4` bytes 就中止並拒收。

沒有沙箱可用時（Windows，或套用沙箱失敗）會以 `sandbox_unavailable` 拒收。願意承擔風險的管理員可以設 `extract_when_unavailable = "run_unsandboxed"`，這時仍有時間上限（macOS 與 Windows 沒有記憶體上限），來源會帶 `extract_unsandboxed` 警告。`.xlsx` 工作表逐格讀取，非空白儲存格上限 200 萬格，宣告超大範圍的工作表耗不光記憶體。同一時間只跑一個解析。

段落保留文件原本的字元（全形標點仍是全形）。只為了比對，每一段另外存一份 NFKC 加小寫的版本（全形英數字、PDF 字型裡的康熙部首都會轉成一般字），所以「ＨＲ」和「HR」互相找得到。文字切成約 500 字、彼此略有重疊的段落：PDF 不跨頁（頁尾太短的殘段併到下一頁的第一段），工作表只在列與列之間切開，每一段都附上表頭列。每一段都會檢查是否像提示注入的文字，命中的段落會被標記並顯示警告。

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

只有在知識庫啟用、且員工這一側至少有一個集合（授權給它或它的部門，再依 `[knowledge]` 縮小）時，這三支工具才存在；列出與呼叫用同一個判斷，授權或撤銷集合後，進行中的對話也會更新（`notifications/tools/list_changed`）。結果裡的每一段內容、集合名稱與說明都放在有標記的區塊內，每行以 `│ ` 開頭，文件裡的文字無法偽裝成結果標題或提早結束區塊。工具透過 gateway 的簽章本機路由查詢；gateway 沒在執行時會回報原因，絕不直接讀資料庫。

## 稽核

- 每次搜尋、讀段落、列清單、下載原檔都寫一筆到 `knowledge.db` 的存取紀錄：誰（儀表板使用者、員工、通道）、查詢內容（前 500 字）、可讀的集合、結果的 id，不存段落內文。超過 `access_log_retention_days` 的紀錄每十分鐘清理一次。
- 建立、修改、刪除集合，修改權限，刪除來源，同時寫進 `security_audit.jsonl`。
- 集合擁有者可以用 `knowledge.access_log.list` 查看存取紀錄：誰、何時、哪位員工或哪個通道、拿到幾段，以及問題的短雜湊。問題原文只有管理員與提問者本人看得到。
- 管理員可以用 `knowledge.access_log.denied` 查看所有集合被拒絕的讀取。
- 權限頁的選擇器用 `knowledge.principals.search`（只回顯示名稱與部門，不回 email；同事只在搜尋字數至少 2 個時回傳，最多 20 位；已在清單上的同事用 `resolve` 依 id 查名稱）；上傳前的檢查用 `knowledge.limits`；員工編輯頁用 `knowledge.agent.collections` 讀取已分享給該員工的集合（只回 id、代號、名稱、敏感度）。

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
max_collection_bytes = 2147483648 # 每個集合原檔總量 2 GB
max_total_bytes = 21474836480     # 全部合計 20 GB
extract_when_unavailable = "refuse"  # 或 "run_unsandboxed"
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

所有資料都在 `<home>/knowledge/`：`knowledge.db`，以及 `blobs/` 底下以 SHA-256 命名的原檔。刪除來源時，段落、索引與向量在同一個交易內刪除，沒有其他集合使用的原檔也一併刪除（十分鐘內才上傳的檔案改由下一次清理刪除，最晚約二十分鐘，因為同一份檔案可能正在上傳）；資料庫開啟 `secure_delete`。刪除集合必須輸入集合名稱確認。

## 相關文件

- [MCP 工具](../../guides/zh-TW/mcp-tools.md)
- [安全防護](05-security-defense.md)
- [儀表板設定](../../guides/zh-TW/dashboard-settings.md)
