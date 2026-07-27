---
name: pptx
description: Read, create, and convert Microsoft PowerPoint (.pptx) presentations — extract slide text to JSON/markdown, build decks from JSON/markdown outlines, and export to PDF.
trigger: pptx, powerpoint, 簡報, 投影片, slides, deck, presentation
tags: [office, document, pptx, powerpoint, slides]
display:
  zh-TW:
    name: PowerPoint 簡報處理
    description: 讀取、建立、轉換 PowerPoint（.pptx）— 抽取投影片文字成 JSON/markdown、用大綱建立簡報、匯出 PDF。
  en:
    name: PowerPoint deck toolkit
    description: Read, create, and convert PowerPoint (.pptx) presentations.
---

# PowerPoint (.pptx) 簡報處理

處理簡報的三件事：**讀取抽取**、**建立**、**轉換 PDF**。腳本用 `uv run` 執行，依賴以
PEP 723 inline metadata 宣告（`python-pptx`）；`uv` 不存在時改用
`pip install python-pptx` 後 `python3` 執行。

## 何時使用

- 收到 `.pptx` 附件，需要讀出各投影片文字來彙總或改寫。
- 要把大綱/重點產出成一份簡報回傳。
- 需要把簡報轉成 PDF 交付。

## 腳本

腳本位於本技能的 `scripts/` 目錄。**兩種執行路徑，依你有沒有 Bash 工具擇一：**

- **有 Bash / shell 工具** → 直接跑 `uv run scripts/<script>.py ...`（見下方各節）。
- **沒有 Bash / shell 工具（API 模式後端，如 Grok / DeepSeek / MiniMax）** → **不要**只回文字大綱，
  改用 `office_script` MCP 工具在伺服器端跑同一支腳本：
  - `skill`：`pptx`
  - `script`：`create` / `extract` / `to_pdf`（不含路徑，`.py` 可省略）
  - `args`：字串陣列，等同 `uv run` 後面那串參數；任何路徑須落在你的 agent 目錄或其 `attachments/`。

  例（把大綱做成簡報）：

  ```json
  {"skill": "pptx", "script": "create",
   "args": ["outline.md", "--out", "/你的agent目錄/attachments/deck.pptx"]}
  ```

  工具以 `uv run`（uv 不存在時退回 `python3`）在你的 agent 目錄內執行並回傳腳本 stdout；
  產出檔案後**務必**依下方 📎DELIVER 協定交付。

### 1. 讀取抽取 — `extract.py`

```bash
uv run scripts/extract.py <input.pptx> --format json   # 每張投影片 → 文字段落陣列
uv run scripts/extract.py <input.pptx> --format md      # 每張投影片 → markdown 段落
```

JSON 輸出 `{"slides": [{"index": 1, "texts": [...]}, ...]}`。

### 2. 建立 — `create.py`

從 markdown 大綱或 JSON 建立 `.pptx`（每個 `#` 標題起一張新投影片，`- ` 為條列）：

來源型別依副檔名判定（`.json` → JSON，其餘 → markdown）：

```bash
uv run scripts/create.py outline.md --out /abs/out.pptx
uv run scripts/create.py deck.json  --out /abs/out.pptx
```

JSON schema：`{"slides": [{"title": str, "bullets": [str, ...]}, ...]}`。

### 3. 轉 PDF — `to_pdf.py`

```bash
uv run scripts/to_pdf.py <input.pptx> --outdir <dir>
```

**未安裝 LibreOffice（`soffice`）時**明確回報未安裝、僅轉換功能不可用（優雅降級）。

## 交付檔案給使用者（📎DELIVER 協定）

產出後在回覆最後另起一行：

```
📎DELIVER:/絕對路徑/deck.pptx
```

路徑須為絕對路徑且位於你的 agent 工作目錄（或其 `attachments/`）下；標記行不顯示給
使用者，請另用文字說明。

**API 模式同樣適用**：用 `office_script` 產出檔案後，一樣在回覆最後一行輸出
`📎DELIVER:<絕對路徑>`，gateway 才會把 `.pptx` 傳回使用者——只回文字大綱不算完成。
