# 問題回報與建議網頁（GitHub Pages + Haiku 自動整理）

讓終端使用者不用懂 GitHub 也能回報問題：填一張中文表單，內容自動整理成
GitHub issue，Haiku 負責分類、上標籤、格式化。全程零自建伺服器。

- **表單網址**：<https://zhixuli0406.github.io/DuDuClaw/>
- **回報去向**：本 repo 的 [Issues](https://github.com/zhixuli0406/DuDuClaw/issues)（`feedback` 標籤）

## 運作流程

```
使用者填表單（GitHub Pages 靜態頁，零秘密）
   │  組出 Markdown，導向 GitHub issue 預填頁
   ▼
使用者在 GitHub 按送出（可拖曳截圖/影片，GitHub 原生上傳）
   │  issue body 帶有 <!-- duduclaw-feedback-form v1 --> 標記
   ▼
GitHub Actions（feedback-triage.yml，僅處理帶標記的 issue）
   │  claude-haiku-4-5 + structured outputs：分類/嚴重度/標題/格式化
   ▼
自動改寫 issue：整理後內容 + 原文收進 <details> + 標籤（feedback + 分類）
```

## 相關檔案

| 檔案 | 作用 |
| --- | --- |
| `feedback/index.html` | 表單頁本體（自包含 HTML，無外部依賴；樣式為 MDS 設計系統的手刻版） |
| `feedback/inter-latin-wght-normal.woff2` | Inter Variable 字體（latin subset，自帶不走 CDN） |
| `.github/workflows/deploy-feedback-page.yml` | `feedback/**` 變更時部署到 GitHub Pages |
| `.github/workflows/feedback-triage.yml` | issue 開立時觸發 Haiku 整理 |

## 設定需求（一次性）

1. GitHub Pages 已設為 workflow 模式（`gh api -X POST repos/<owner>/DuDuClaw/pages -f build_type=workflow`）。
2. Repo secret `ANTHROPIC_API_KEY`：`gh secret set ANTHROPIC_API_KEY`。
   沒設的話 triage 會直接略過（issue 原樣保留），表單流程不受影響。
3. `feedback` 標籤（已建立；砍掉的話 triage 上標籤會失敗）。

## 安全設計

- **前端零秘密**：API key 與 token 都只存在 Actions secrets，靜態頁拿不到。
- **Prompt injection 防護**：issue 內容以 XML 標籤包裹並明確降格為資料；
  模型輸出受 JSON schema 約束（分類只能落在四個 enum 值）；原文永遠保留在
  `<details>`，整理失敗時 issue 原樣不動。
- **Script injection 防護**：workflow 不把 issue body 插進 shell，改用
  `gh api` 抓進檔案、`jq` 組 JSON。
- **成本**：只有帶表單標記的 issue 會觸發，輸入截斷 16k 字元，單次呼叫
  Haiku 成本約 $0.01 以下。

## 修改表單

改 `feedback/index.html` 後 push 到 main 即自動重新部署。欄位變動時記得同步
`feedback-triage.yml` 內 system prompt 的段落名稱（問題描述／重現步驟／預期行為／環境）。

樣式遵循 MDS 設計系統（與 dashboard `web/src/components/mds/` 同源）：OKLCH
色彩 token、surface 分層、radius 體系（按鈕／輸入 10px、卡片 14px）、Inter +
繁中系統字 fallback、字重只用 400/500、brand 藍 CTA、focus ring 3px。因為是
無 build 的靜態頁，token 直接以 CSS custom properties 手刻在 `<style>` 內，
改動 MDS token 時需手動同步。深色模式走 `prefers-color-scheme`（dashboard 的
`.dark` class 機制在靜態頁不適用）。
