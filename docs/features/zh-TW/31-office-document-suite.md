# Office 文件套件

> Agent 交到你手上的是真正的 .docx,不是一段描述它的文字:靠一套 marker 協議、一道 fail-closed 路徑圍欄,以及模型忘了 marker 那天用的安全網。

---

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

回覆文字是所有 runtime 共用的唯一交接點:Claude CLI、Codex、Gemini、direct API。在回覆裡放 marker,讓*模型*宣告意圖,而*gateway* 保有「什麼東西真的離開沙盒」的最終權威。另一條路(由 gateway 從散文推斷交付物)正是這套協議要避免的失效模式。

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
- `docx/xlsx/pptx/odt/ods/odp/csv` 由 LibreOffice(`soffice --headless`)轉成 PDF,快取在 `<home>/cache/preview/<agent>/` 之下並以 mtime 驗證,使用隔離的 LO profile(並行轉檔會搶預設 profile 的鎖),60 秒逾時。
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
