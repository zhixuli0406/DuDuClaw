---
name: pdf
description: Read and create PDF documents — extract text (and per-page text) from a PDF to JSON/markdown, and render markdown/plain text into a new PDF.
trigger: pdf, 檔案, extract text, 抽取, 合併, merge, 產出 pdf
tags: [office, document, pdf]
display:
  zh-TW:
    name: PDF 文件處理
    description: 讀取與建立 PDF — 從 PDF 抽取文字（含逐頁）成 JSON/markdown，並把 markdown/純文字產成新的 PDF。
  en:
    name: PDF document toolkit
    description: Read and create PDF documents.
---

# PDF 文件處理

處理 PDF 的兩件事：**讀取抽取**與**建立**。腳本用 `uv run` 執行，依賴以 PEP 723
inline metadata 宣告（讀取用 `pypdf`、建立用 `reportlab`）；`uv` 不存在時改用
`pip install pypdf reportlab` 後 `python3` 執行。

## 何時使用

- 收到 `.pdf` 附件，需要讀出文字來彙總、擷取、分析。
- 要把整理好的文字/報告直接產成一份 PDF 回傳。

## 腳本

腳本位於本技能的 `scripts/` 目錄。**兩種執行路徑，依你有沒有 Bash 工具擇一：**

- **有 Bash / shell 工具** → 直接跑 `uv run scripts/<script>.py ...`（見下方各節）。
- **沒有 Bash / shell 工具（API 模式後端，如 Grok / DeepSeek / MiniMax）** → **不要**只回文字，
  改用 `office_script` MCP 工具在伺服器端跑同一支腳本：
  - `skill`：`pdf`
  - `script`：`create` / `extract`（不含路徑，`.py` 可省略）
  - `args`：字串陣列，等同 `uv run` 後面那串參數；任何路徑須落在你的 agent 目錄或其 `attachments/`。

  例（把整理好的文字產成 PDF）：

  ```json
  {"skill": "pdf", "script": "create",
   "args": ["report.md", "--out", "/你的agent目錄/attachments/report.pdf"]}
  ```

  工具以 `uv run`（uv 不存在時退回 `python3`）在你的 agent 目錄內執行並回傳腳本 stdout；
  產出檔案後**務必**依下方 📎DELIVER 協定交付。

### 1. 讀取抽取 — `extract.py`

```bash
uv run scripts/extract.py <input.pdf> --format json   # {"pages": ["p1 text", ...]}
uv run scripts/extract.py <input.pdf> --format md      # 逐頁文字，以 --- 分隔
```

### 2. 建立 — `create.py`

把 markdown 或純文字產成 PDF（`#`/`##` 標題會用較大字級，其餘為內文；CJK 以內建
字型排版）：

來源型別依副檔名判定（`.txt` → 純文字，其餘 → markdown）：

```bash
uv run scripts/create.py report.md --out /abs/out.pdf
uv run scripts/create.py notes.txt --out /abs/out.pdf
```

> 若需要把 Word/Excel/PPT 轉成 PDF，請改用對應的 `docx` / `xlsx` / `pptx` 技能的
> `to_pdf.py`（LibreOffice headless）。

## 交付檔案給使用者（📎DELIVER 協定）

產出後在回覆最後另起一行：

```
📎DELIVER:/絕對路徑/report.pdf
```

路徑須為絕對路徑且位於你的 agent 工作目錄（或其 `attachments/`）下；標記行不顯示給
使用者，請另用文字說明。

**API 模式同樣適用**：用 `office_script` 產出 `.pdf` 後，一樣在最後一行輸出
`📎DELIVER:<絕對路徑>`——只回文字不算完成。
