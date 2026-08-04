---
name: docx
description: Read, create, and convert Microsoft Word (.docx) documents — extract text and tables, build reports from markdown/JSON, and export to PDF.
trigger: docx, word, 報告, 公文, 文件, 合約, contract, report
tags: [office, document, docx, word]
display:
  zh-TW:
    name: Word 文件處理
    description: 讀取、建立、轉換 Word（.docx）文件 — 抽取文字與表格、用 markdown/JSON 生成報告、匯出 PDF。
  en:
    name: Word document toolkit
    description: Read, create, and convert Word (.docx) documents.
---

# Word (.docx) 文件處理

處理 `.docx` 檔案的三件事：**讀取抽取**、**建立**、**轉換 PDF**。所有腳本用
`uv run` 執行，依賴以 PEP 723 inline metadata 宣告（`python-docx`），uv 會自動
建立隔離環境；`uv` 不存在時改用 `pip install python-docx` 後 `python3` 執行。

## 何時使用

- 收到 `.docx` 附件，需要讀出內容或表格來彙總、分析。
- 要把彙總結果、報告產出成一份 Word 文件回傳給使用者。
- 需要把 Word 轉成 PDF 交付。

## 腳本

腳本位於本技能的 `scripts/` 目錄（相對於此 SKILL.md）。**兩種執行路徑，依你有沒有
Bash 工具擇一：**

- **有 Bash / shell 工具** → 直接跑 `uv run scripts/<script>.py ...`（見下方各節）。
- **沒有 Bash / shell 工具（API 模式後端，如 Grok / DeepSeek / MiniMax）** → **不要**只回文字，
  改用 `office_script` MCP 工具在伺服器端跑同一支腳本：
  - `skill`：`docx`
  - `script`：`create` / `extract` / `to_pdf`（不含路徑，`.py` 可省略）
  - `args`：字串陣列，等同 `uv run` 後面那串參數；任何路徑須落在你的 agent 目錄或其 `attachments/`。

  例（把報告做成 Word）：

  ```json
  {"skill": "docx", "script": "create",
   "args": ["report.md", "--out", "/你的agent目錄/attachments/report.docx"]}
  ```

  工具以 `uv run`（uv 不存在時退回 `python3`）在你的 agent 目錄內執行並回傳腳本 stdout；
  產出檔案後**務必**依下方 📎DELIVER 協定交付。

### 1. 讀取抽取 — `extract.py`

```bash
uv run scripts/extract.py <input.docx> --format json   # 依文件順序的 blocks
uv run scripts/extract.py <input.docx> --format md      # markdown 純文字
```

`json` 輸出 `{"blocks": [{"type":"heading"|"paragraph"|"bullet"|"table", ...}]}`，
**依 document body 的實際順序**（標題／段落／清單／表格交錯，不重新分組）；`md`
把同一組 blocks 轉成可直接閱讀的 markdown。此 JSON 即 `create.py` 的輸入格式，
故 extract → create 可往返。

### 2. 建立 — `create.py`

從 markdown 或 JSON 來源檔建立 `.docx`（來源型別依副檔名判定，`.json` → JSON，
其餘 → markdown）：

```bash
uv run scripts/create.py report.md --out /abs/path/report.docx
uv run scripts/create.py spec.json --out /abs/path/report.docx
```

markdown 支援 `#`/`##`/`###` 標題、清單 `- `、以及 `|` 分隔的表格。

### 3. 轉 PDF — `to_pdf.py`

用 LibreOffice headless 轉換：

```bash
uv run scripts/to_pdf.py <input.docx> --outdir <dir>
```

**未安裝 LibreOffice（`soffice`）時**，腳本會明確回報「LibreOffice 未安裝，僅轉換
功能不可用；讀取與建立功能不受影響」並以非零碼結束 — 不是靜默失敗。

## 交付檔案給使用者（📎DELIVER 協定）

產出檔案後，**在回覆的最後另起一行**加上標記，gateway 會自動把該檔案傳回使用者：

```
📎DELIVER:/絕對路徑/report.docx
```

規則：
- 路徑必須是**絕對路徑**，且位於你的 agent 工作目錄（或其 `attachments/`）下。
- 一行一個檔案，可多行。
- 標記行不會顯示給使用者；請同時用一般文字說明你做了什麼。
- **API 模式同樣適用**：用 `office_script` 產出 `.docx` 後，一樣在最後一行輸出
  `📎DELIVER:<絕對路徑>`——只回文字不算完成。
