# Office 文件套件

四項辦公技能只在公司層 `<home>/skills/` 安裝一次，不再複製到每位新員工。既有副本會保留；程式技能選用與清理方式見[技能生命週期](15-skill-lifecycle.md)。

> Agent 交到你手上的是真正的 .docx,不是一段描述它的文字:靠一套 marker 協議、一道 fail-closed 路徑圍欄,以及模型忘了 marker 那天用的安全網。

---

## 辦公文件腳本的信任範圍

`office_script` 只執行目前主程式內嵌的 Python 腳本，每支腳本都必須符合編譯時的 SHA-256 manifest；主程式版本與腳本雜湊共同識別版本。個人、部門與公司技能只能覆蓋 **SKILL.md 提示文字**，即使目錄裡有同名腳本，也不會拿來執行。

每次呼叫都由 gateway／MCP 行程在 `<home>/.office-execution/` 下建立屬於自身 uid、權限 `0700` 的私有目錄，以排他建立、no-follow 與 `0500` 權限寫入已驗證的腳本。輸入檔逐層以 no-follow 開啟、確認正規路徑未越界後，複製到私有目錄。既有文件資源限制檢查同一份快照；Python 在該目錄執行，清除繼承環境，不讀員工的專案設定或匯入員工目錄的模組。

參數按腳本分別驗證：只能有一份輸入；`create` 必填 `--out`；`to_pdf` 可填 `--outdir`；`--format` 只接受該腳本列出的值。旗標和值須分開傳送，例如 `"--out", "report.pdf"`；拒絕 `--out=/路徑`、未知或重複旗標及多餘輸入。輸入限員工自己的目錄或 `<home>/attachments/`；輸出限**員工自己目錄下的新檔案**，副檔名須符合文件格式，父目錄須已存在。既有檔案、受保護結構檔名、符號連結、硬連結與路徑穿越均拒絕。轉 PDF 未指定 `--outdir` 時採原輸入檔的父目錄，該目錄也必須位於員工目錄內。Python 只寫暫存輸出，成功後由 Rust 重新逐層檢查目的地，先在私有暫存區完成複製與同步，再以不可覆寫的排他 rename 寫入經 no-follow 開啟的目的目錄；暫存區與輸出必須位於同一檔案系統，失敗時不留下部分目的檔。

技能層根目錄與各路徑元件均拒絕符號連結。部門提示必須由權威 `org.toml` 查到，且與 `agent.toml` 副本一致；資料無法讀取或不一致時不載入部門層。四個公司技能目錄的腳本全部符合 manifest，才寫入辦公技能 migration 完成標記。衝突副本會保留、排除於執行來源，並寫入日誌及顯示於 `duduclaw doctor`；舊 migration 標記不代表腳本已驗證。

既有 MCP 權限、核准、技能停用與文件資源限制維持有效。`office_script` 僅支援 Linux 與 macOS；檔案系統、網路及記憶體隔離未全部成立時，預設拒絕執行。Linux 需要 bwrap，僅將系統／runtime／輸入唯讀掛載，私有輸出與快取可寫，強制網路／PID namespace，並套用 2 GiB RLIMIT_AS。macOS 使用 deny-by-default 的 sandbox-exec profile，禁止網路、限制讀寫範圍，嘗試 DATA／RSS 上限並探測是否真的拒絕超限配置；無法套用或無效即拒絕。CPU／檔案／程序／fd 限制及 process group 清理保留。host 暖快取步驟只在獨立暫存目錄執行已驗證 wrapper 的 `--help`，不提供使用者檔案；僅此步驟可連網下載依賴到 `<home>/.office-execution/uv-base-v1/`。沙箱將 base 掛為唯讀，並以 `UV_CACHE_DIR` 指向 `<私人工作區>/uv-cache` 的獨立副本，搭配 `UV_LINK_MODE=copy` 與離線模式。快取連結會改指向副本，執行環境在副本內離線重建。工作區、輸出暫存與快取於發布或失敗後清除；舊的解析器可寫快取不會升格為可信 base，明確 unsandboxed fallback 保留原快取行為。沿用 `config.toml [container.sandbox] script_when_unavailable = "run_unsandboxed"`，僅在管理員明確設定時允許隔離不可用的 fallback，每個主機程序警告一次；`sandbox_enabled = false` 不構成隱含豁免。同 UID 的 pathname／快取競態，以及遞迴依賴尚無完整 hash lock 的殘餘風險，仍記錄於[安全防護](../05-security-defense.md)。Linux／macOS 隔離成功路徑、真實 LibreOffice 與 gateway／CLI 整合須另行驗收；本機開發環境拒絕 sandbox-exec，不能視為成功隔離驗證。

執行器已固定直接依賴版本及套件發布截止時間，禁止 source build／自動下載 Python；uv 在不含使用者輸入的 host 暖快取完成後採離線模式，system Python 必須符合直接依賴版本。Unix 資源限制及 stdout／stderr 各 32 KiB 串流上限會在超限或逾時時終止群組；執行前重驗路徑與 hash。仍未提供完整遞迴依賴 hash 與同 UID 主機程序隔離；明確選用 unsandboxed fallback 時也不保證斷網。精確限制、快取復原及剩餘風險見 [Security Defense](../05-security-defense.md#office-parser-limits-and-remaining-trust-boundary)。

建立 Linux 掛載或 macOS profile 前，會解析 home、工作／輸出目錄、輸入、腳本、base／單次快取及所有現存 runtime 允許根目錄的 canonical 路徑。私人路徑與 runtime 根目錄（含原始別名及解析目標）只要相等，或任一方是另一方的祖先，就拒絕隔離；解析失敗也會拒絕。base 不得與可寫工作區重疊，單次快取必須位於本次工作區內。這可避免 `/usr/local`、`/opt/homebrew` 下的自訂 home，或移入 runtime 目錄的私人工作區／快取，透過廣泛讀取權限暴露其他員工。只有既有的 `script_when_unavailable = "run_unsandboxed"` 明確豁免可允許降級，並沿用原有警告。

## 這是什麼

DuDuClaw 的 agent 能產出真正的辦公檔案:Word(`.docx`)、Excel(`.xlsx`)、PowerPoint(`.pptx`)、PDF,並以原生附件形式,送回發出請求的那個聊天通道。四個內建文件 skill 教 agent 怎麼組出各個格式;檔案存在之後的一切歸 gateway 管:驗證、通道遞送、歸檔、儀表板預覽。

套件也處理入站方向。當訊息帶著文件附件進來,一張確定性的副檔名對照表(`office_docs.rs`)把它對應到正確的 skill:`doc`/`docx` → docx、`xls`/`csv`/`xlsx` → xlsx、`ppt`/`pptx` → pptx、`pdf` → pdf,並把該 skill 強制加入 progressive skill ranker 的作用集。純查表,沒附文件時零成本。

## 📎DELIVER 協議

產出檔案後,agent 在回覆末尾為每個檔案附上一行 marker:

```
📎DELIVER:/home/u/.duduclaw/agents/sales/q3-report.docx
```

接著 gateway 會:

1. 從使用者可見的文字中剝除所有 marker 行。
2. 以 **fail-closed** 方式驗證路徑:必須是絕對路徑、存在且為一般檔案,並且在 canonicalization(解析 `..` 與 symlink)之後,落在該 agent 自己的目錄或共用的 `attachments/` fallback 之內。像 `agents/me/../victim/secret` 這種 traversal 在 canonicalize 後跑出 agent root,會被拒絕。
3. 讀出位元組,經由該通道的 `send_document` 送出。

沒有 marker 的回覆逐位元組原樣返回——最常見的情況保持 prompt cache 與排版穩定。任何驗證、讀取或送出失敗都誠實降級:回覆後會附上一段點名檔案所在位置的文字,使用者永遠不會納悶交付物跑哪去了。靜默丟棄不是一種結果。

## 為什麼用 marker,不用猜的

回覆文字是所有 runtime 共用的唯一交接點:Claude CLI、Codex、direct API。在回覆裡放 marker,讓*模型*宣告意圖,而*gateway* 保有「什麼東西真的離開沙盒」的最終權威。另一條路(由 gateway 從散文推斷交付物)正是這套協議要避免的失效模式。

這套協議原本只寫在 office skill 的 SKILL.md 裡,一次真實事故(2026-07-28)暴露了缺口:agent 產出了真正的 `.docx`,寫到 `~/Desktop`,卻始終沒有輸出 marker。使用者收到一段聲稱檔案存在的文字;gateway 沒有東西可送、可歸檔。於是落地了兩個修法:

- **常駐系統規則**(`channel_reply.rs` 的 `deliver_rules`):每個通道 system prompt 裡的一段靜態、prompt-cache 友善的區塊;檔案必須存進工作目錄、每個檔案要有自己的 `📎DELIVER:` 行,而且「用文字描述檔案」不算交付。同級的另一段常駐區塊(`pacing_rules`)修另一則實地回報:重任務輪之後,一句單純的打招呼應該得到簡短回覆,不是把前一個任務重跑一遍。
- **下述的 sweep**,補上 prompt 規則保證不了的那一半。

## Sweep 安全網

`sweep_undeclared_deliverables` 是「檔案寫進了工作目錄但忘了 marker」的確定性補網。當一則沒有 marker 的回覆*談到*產出了文件(關鍵詞閘:副檔名、"Word"/"Excel"/"PowerPoint"、zh-TW 詞彙如 檔案/簡報/報表),gateway 掃描 agent 目錄,把找到的東西照宣告過一樣遞送:

| 條件 | 值 |
|---|---|
| 檔案類型 | 僅 `docx xlsx pptx pdf csv odt ods odp`,絕不含 `.md`/`.txt`/`.json`(agent 把那些當內部狀態在寫) |
| 時間窗 | mtime 在 15 分鐘內 |
| 遞迴深度 | ≤ 3,跳過 `attachments/`、`sessions/`、`logs/`、`memory/` 與隱藏目錄 |
| 每回覆上限 | 3 個檔案,最新優先 |
| 去重 | sanitized 檔名 + 大小已存在於歸檔者,代表先前輪次已遞送過,跳過 |

關鍵詞閘是遞送啟發式,不是安全決策;圍欄始終是 `validate_deliver_path`。sweep 的失敗會記錄後跳過。這張網永遠不會變成回覆本身的新失效模式。

## 歸檔、Files 頁、預覽

每個遞送的檔案在通道送出**之前**先複製進該 agent 的 `attachments/` 目錄,所以即使送出失敗,交付物仍能在儀表板的 Files 頁瀏覽(本來就在 `attachments/` 內的檔案不會重複複製)。

Files 頁透過 `GET /api/files/preview` 在瀏覽器裡預覽辦公文件:

- PDF 與圖片原生 inline 串流。
- `docx/xlsx/pptx/odt/ods/odp/csv` 由 LibreOffice（`soffice --headless`）將已驗證快照轉成 PDF，每次請求使用私有目錄及獨立 LO profile，60 秒逾時。轉檔不再讀取可變的附件路徑或沿用 mtime 快取。來源與轉檔結果各限 2 MiB；較大檔案可下載，上限 20 MiB。一般下載與直接預覽也對實際回傳的同一份快照驗證內容和權限。這些附件操作需要 Unix no-follow 目錄控制代碼，其他平台拒絕存取。
- 缺 LibreOffice → 明確的 503 JSON 訊息,絕不吐出壞掉的位元組串流。
- 與下載端點相同的 JWT 驗證與路徑圍欄。

## Provenance (I-2b)

每個進到 `attachments/` 的檔案,以前只是扁平目錄清單裡一筆匿名項目;客戶的入站上傳,和 agent 為某個 goal task 產出的報表,兩者難以分辨。一份 provenance ledger(`artifacts.jsonl`,與 `tool_calls.jsonl`、`task_changes.jsonl` 放在一起)現在會記錄每個檔案從哪裡來,**在寫入當下**就記下,絕不事後用猜的:

| 來源 | 意義 |
|---|---|
| `declared` | agent 透過 `📎DELIVER:` 親手交付。 |
| `swept` | `sweep_undeclared_deliverables` 撿回來的;檔案存在,只是 marker 被忘了。 |
| `uploaded` | 人類透過某個通道送進來的(入站附件)。 |
| `produced` | 在讀取時從任務自己的 change ledger 推導出來,從未真正寫進 artifacts 檔案本身。 |
| `unknown` | 證據不足以判斷。誠實留白,不亂猜。 |

任務歸屬分兩層,標法相同:當該筆記錄(或任務自己的 change ledger)直接點名任務時記為 `exact`;只靠 claim→review 時間窗慣例(與 `tasks.changes` 用的同一個窗口)推定出來時記為 `inferred`,dashboard 絕不會把推論當成事實呈現。合併用的 key 是檔案真正的 basename,不是 CJK-sanitized 過的顯示名稱,因此兩個來源不同、卻剛好 sanitize 成同一字串的檔案,永遠不會被併成一筆。開機時的 backfill 會替 ledger 出現之前的檔案補齊歷史,但只補到既有證據(declared/swept 紀錄、`task_changes.jsonl`)所能到達的範圍;放不進去的一律留 `unknown`,不猜方向。

有兩個介面會讀這份 ledger:

- **任務詳情 → 產物分頁**:列出每個與該任務綁定的檔案,含來源、輪次,以及(對有歸檔副本的 `uploaded`/`declared`/`swept` 列)一個下載連結。agent 寫過但從未歸檔的檔案,會顯示它被寫入的路徑,而不是一個失效的下載連結。
- **Files 頁**:一欄來源加上任務篩選器,「找上週的交付物」變成一次篩選,不必再滾頁面找。

**已知限制**:goal-loop 的 dispatch 路徑,目前還不會替它產出的檔案歸檔一份可下載的副本;這些列可能會以 `produced` 的樣子出現,背後卻沒有附件。補上這個缺口已排進未來的 wave。

## 限制

| 項目 | 限制 |
|---|---|
| 遞送檔案大小 | 20 MB(`media::MAX_FILE_SIZE`) |
| 遞送路徑 | 絕對路徑、canonicalized、位於 agent 目錄或共用 `attachments/` 內(fail-closed) |
| Sweep | 每回覆 3 個檔案、15 分鐘窗、深度 ≤ 3、僅 office 副檔名 |
| 預覽轉檔 | office 類型需要 LibreOffice;每次轉檔 60 秒逾時 |


### 歷史對話中的附件

Unix gateway 的歷史對話會將可讀取的附件顯示為檔案卡，包含名稱、大小、類型與下載按鈕。PNG、JPEG、GIF、WebP 提供縮圖；PDF 與 Office 檔案下載後使用文件閱讀器開啟，歷史頁不執行 LibreOffice 轉檔。「檔案」頁保留原有預覽方式。

既有 Markdown 附件連結由伺服器解析；DELIVER 若指向工作目錄，必須有明確且唯一的歸檔紀錄，才能取得該員工附件目錄中的副本。檔案已刪除、搬家後仍指向舊主機的路徑、重複使用來源路徑而無法確認版本，以及不安全路徑都不提供下載卡。瀏覽器只收到不透明識別碼，不會收到附件連結的主機路徑。

每次取檔沿用目前登入者的 Authorization header，同時檢查對話歷史與既有檔案擁有者／受眾權限，網址不含登入憑證。外部對話的遮罩模式不提供原始附件；受限讀者仍須有可信 WebChat 擁有者紀錄或既有任務授權，不能藉由訊息內手寫路徑取得他人檔案。識別碼在 30 分鐘後或 gateway 重啟後失效，重新載入歷史即可更新。非 Unix gateway 尚未實作以目錄描述符防止符號連結競態的讀檔，因此此新端點目前拒絕取檔。

下載上限為 20 MiB，圖片預覽上限為 2 MiB。較大的圖片只提供下載，卡片接近畫面時不會自動取檔。Gateway 檢查已開啟檔案的大小，建立有大小上限的快照，再將其雜湊與來源紀錄及必要的工作流程收據比對，最後回傳同一份內容。回應帶有 `Content-Length`，瀏覽器也會檢查長度，超過上限即停止讀取。上傳使用不可預測的 UUID 檔名，僅可建立新檔，因此同時上傳同名檔案不會互相覆寫。

歷史查詢以 SQL `LIMIT` 直接讀取最新訊息，每次請求開啟一次耐久授權索引，按歸檔名稱與來源路徑查詢。每次回應的訊息文字總量最多 2 MiB、單則最多 1 MiB，最多提供 32 個附件、檢查 256 個參照。歷史投影每位使用者最多同時 2 個請求、每分鐘 30 次，全域最多同時 4 個。下載每位使用者最多同時 2 個、全域最多同時 4 個；每位使用者每分鐘有 120 MiB 額度，依該次請求的大小上限預留，而非依實際傳輸量扣除。超過限制時該次請求無法使用，請稍後重試。

附件識別碼使用獨立快取，每位使用者最多保存 512 筆，優先淘汰最久未使用的項目，因此部分識別碼可能在 30 分鐘期限前失效。全域快取無法保存時，不會回傳無法使用的識別碼。缺少內容雜湊的舊產物不提供受限讀者下載；Admin／完整歷史模式原有的舊檔規則維持。Windows 與其他非 Unix gateway 拒絕此端點。本輪修正尚未完成原生平台行為與實際瀏覽器下載驗收。

WebChat 在寫檔或分派工作前驗證整批上傳：每批最多 8 個附件、解碼後合計 20 MiB，檔名最多 255 bytes、MIME 標籤最多 128 bytes。WebSocket 訊息及 frame 上限為 28 MiB；附件解析器會在建立過大的附件陣列前拒絕超量輸入。Base64 不合法或超過限制時，整批拒絕。每位使用者每分鐘最多 8 批、32 個檔案及 80 MiB。持久化預留配額限制新上傳：每位使用者 512 MiB／2,048 個檔案，全域 4 GiB／16,384 個檔案。空檔案仍占用檔案數額度；失敗或中斷的預留仍計入配額，手動刪檔不會自動恢復額度。

下載回應有 30 秒總期限。即使客戶端完全不讀取內容，獨立計時器仍會釋放快照與下載槽。傳輸層每次取得最多 64 KiB 的獨立副本，因此保留一個區塊不會同時保留整份快照。到期的回應會失敗；重試須重新通過授權，並按原有規則扣除請求額度。

歷史附件授權使用有索引的耐久 SQLite 紀錄，保存歸檔名稱、擁有者、內容雜湊、大小及來源路徑。展示／稽核用 JSONL 輪替或截短不再移除既有授權紀錄。資料庫無法使用時拒絕受限存取，WebChat 上傳須成功保存授權紀錄才回報成功。缺少可信內容綁定的舊檔仍不提供受限讀者下載。

首次使用時，會在同一筆交易匯入保留的前一份及目前稽核檔，合計上限 32 MiB、每列上限 64 KiB。來源無法讀取、格式錯誤或超限時，遷移整筆失敗，不產生部分授權；須修復保留紀錄後重試。更早輪替已丟棄的紀錄無法自動重建。匯入後再修改稽核檔，不會覆寫授權資料庫。
