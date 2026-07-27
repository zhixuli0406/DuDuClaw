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

腳本位於本技能的 `scripts/` 目錄。

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
