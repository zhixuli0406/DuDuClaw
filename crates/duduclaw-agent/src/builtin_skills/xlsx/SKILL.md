---
name: xlsx
description: Read, create, and convert Microsoft Excel (.xlsx) and CSV spreadsheets — extract sheets and tables to JSON, build workbooks from JSON/CSV, and export to PDF.
trigger: xlsx, excel, csv, 試算表, 表格, 報表, spreadsheet, 彙總, summary
tags: [office, document, xlsx, excel, csv]
display:
  zh-TW:
    name: Excel 試算表處理
    description: 讀取、建立、轉換 Excel（.xlsx）與 CSV — 抽取工作表與表格成 JSON、用 JSON/CSV 建立活頁簿、匯出 PDF。
  en:
    name: Excel spreadsheet toolkit
    description: Read, create, and convert Excel (.xlsx) and CSV spreadsheets.
---

# Excel (.xlsx) / CSV 試算表處理

處理試算表的三件事：**讀取抽取**、**建立**、**轉換 PDF**。腳本用 `uv run` 執行，
依賴以 PEP 723 inline metadata 宣告（`openpyxl`）；`uv` 不存在時改用
`pip install openpyxl` 後 `python3` 執行。

## 何時使用

- 收到 `.xlsx` / `.csv` 附件，需要讀出資料來彙總、計算、分析。
- 要把彙總或計算結果產出成一份 Excel 活頁簿回傳。
- 需要把試算表轉成 PDF 交付。

## 腳本

腳本位於本技能的 `scripts/` 目錄。**兩種執行路徑，依你有沒有 Bash 工具擇一：**

- **有 Bash / shell 工具** → 直接跑 `uv run scripts/<script>.py ...`（見下方各節）。
- **沒有 Bash / shell 工具（API 模式後端，如 Grok / DeepSeek / MiniMax）** → **不要**只回文字，
  改用 `office_script` MCP 工具在伺服器端跑同一支腳本：
  - `skill`：`xlsx`
  - `script`：`create` / `extract` / `to_pdf`（不含路徑，`.py` 可省略）
  - `args`：字串陣列，等同 `uv run` 後面那串參數；任何路徑須落在你的 agent 目錄或其 `attachments/`。

  例（把彙總資料做成 Excel）：

  ```json
  {"skill": "xlsx", "script": "create",
   "args": ["data.json", "--out", "/你的agent目錄/attachments/summary.xlsx"]}
  ```

  工具以 `uv run`（uv 不存在時退回 `python3`）在你的 agent 目錄內執行並回傳腳本 stdout；
  產出檔案後**務必**依下方 📎DELIVER 協定交付。

### 1. 讀取抽取 — `extract.py`

```bash
uv run scripts/extract.py <input.xlsx> --format json   # 每個工作表 → 二維陣列
uv run scripts/extract.py <input.csv>  --format json
uv run scripts/extract.py <input.xlsx> --format md      # 每個工作表 → markdown table
```

JSON 輸出 `{"sheets": {"Sheet1": [[cell,...],...]}}`，值保留原始型別（數字/字串）。

### 2. 建立 — `create.py`

來源型別依副檔名判定（`.csv` → CSV，其餘 → JSON）：

```bash
uv run scripts/create.py data.json --out /abs/out.xlsx
uv run scripts/create.py data.csv  --out /abs/out.xlsx
```

JSON schema：`{"sheets": {"工作表名": [["表頭",...], [值,...], ...]}}`。第一列會加粗
作為表頭。

### 3. 轉 PDF — `to_pdf.py`

```bash
uv run scripts/to_pdf.py <input.xlsx> --outdir <dir>
```

**未安裝 LibreOffice（`soffice`）時**明確回報未安裝、僅轉換功能不可用，讀取與建立
不受影響（優雅降級，非靜默失敗）。

## 交付檔案給使用者（📎DELIVER 協定）

產出檔案後，在回覆最後另起一行加上標記，gateway 會自動把檔案傳回：

```
📎DELIVER:/絕對路徑/summary.xlsx
```

路徑須為絕對路徑且位於你的 agent 工作目錄（或其 `attachments/`）下；標記行不顯示給
使用者，請另用文字說明。

**API 模式同樣適用**：用 `office_script` 產出 `.xlsx` 後，一樣在最後一行輸出
`📎DELIVER:<絕對路徑>`——只回文字不算完成。
