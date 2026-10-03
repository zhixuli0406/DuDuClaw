# 打造你自己的 Expert Pack（動手教學）

> 對象：想把「一個 AI 員工／一組團隊」包成可分享安裝包的創作者（SI、顧問、社群貢獻者）。
> 這是 tutorial；欄位完整參考見 [features/32-expert-packs.md](../../features/zh-TW/32-expert-packs.md)。

Expert pack 是 DuDuClaw 生態系的統一封裝單位：一個目錄（或 zip／URL），裝著員工的 persona、技能、SOP 知識頁與推薦提示，對方一行指令就能裝進自己的 DuDuClaw。

## 1. 最小可用包（10 分鐘）

建一個目錄：

```
my-first-pack/
├── expert.toml
├── agents/
│   └── helper/
│       ├── soul.md              # 員工 persona（身份/職責/邊界）
│       └── agent.partial.toml   # 選填：deep-merge 進 agent.toml 的片段
└── skills/
    └── greeting/
        └── SKILL.md             # 選填：隨包技能
```

`expert.toml` 最小內容（欄位以 [features/32](../../features/zh-TW/32-expert-packs.md) 為準）：

```toml
[expert]
name = "my-first-pack"
description = "示範：一位友善的小幫手"
version = "0.1.0"
author = "你的名字"
license = "MIT"
tags = ["demo"]
category = "general"

[[expert.agents]]
name = "helper"
role = "main"
display_name = "小幫手"
```

`agents/helper/soul.md` 寫 persona。身份／職責／邊界三段是好起點；邊界寫得越清楚，安裝者越敢用。

### 關於 `pack.toml`

團隊包與產業包請用 `expert.toml`。較新的 `pack.toml` schema 能描述團隊包、產業包與職務 preset，`duduclaw pack inspect` 三種都讀得懂。但 `duduclaw pack install` 目前只能安裝 `kind` 為 `"preset"` 的 `pack.toml`（職務 preset）。`kind = "team"` 或 `"template"` 的 `pack.toml` 可以讀取與檢視，還不能安裝，因為背後的安裝程式只靠 `expert.toml` 辨認團隊包或產業包。因此 `expert.toml` 雖已棄用但仍然讀取，要等 `pack.toml` 的團隊包能安裝之後，才會跟改寫後的付費板模一起移除，目前沒有訂版號。

想看手上的包在新 schema 下長什麼樣？`duduclaw pack inspect <dir> --emit-canonical` 會印出對應的 `pack.toml` 讓你審過再存檔，它不會覆寫你的檔案。

## 2. 本機測試迴路

```bash
# 先看 DuDuClaw 怎麼讀你這包（還沒裝任何東西）
duduclaw pack inspect ./my-first-pack

# 驗證 + 安裝（目錄直接裝）
duduclaw pack install ./my-first-pack

# 看裝了什麼
duduclaw pack list

# 打包成可分享的 zip（製作端動詞留在 `expert` 之下）
duduclaw expert pack ./my-first-pack

# 對方安裝（本機 zip 或 URL 皆可）
duduclaw pack install ./my-first-pack-0.1.0.zip
duduclaw pack install https://example.com/my-first-pack-0.1.0.zip

# 收乾淨（移除 pack 的員工、隨包技能與 wiki 頁）
duduclaw expert remove my-first-pack
```

`duduclaw expert install` 已在 v1.69.0 移除，執行時會印出 `duduclaw pack install` 並以結束碼 2 離開。`expert list` 仍然可用，列出已安裝的紀錄。

安裝端的防護是內建的：zip-slip 圍欄、50MB 上限、內容掃描；**hooks 一律先裝進隔離區**（`hooks-disabled/`），要操作者明確信任才啟用。寫包時別假設 hooks 會自動生效。

## 3. 進階：團隊、知識頁、需求宣告

- **多員工團隊**：多個 `[[expert.agents]]`，用 `reports_to` 組層級（安裝時自動照拓撲順序建立），`department` 分部門。
- **SOP／知識**：`wiki/<namespace>/*.md` 會裝進共享知識庫。法規、話術、價目表放這裡，不要塞進 SOUL。
- **需求宣告**：`[expert.requires]` 的 `env`（需要的環境變數）與 `bins`（需要的外部指令）讓安裝者在裝之前就知道前置條件，免得裝完才踩雷。
- **推薦提示**：`[expert.prompts] recommended` 列 3–5 句「裝完先試這些」，是安裝者的 First-Win。

## 4. 從既有資產轉出

- 手上已有 DuDuClaw 團隊？`duduclaw expert convert-teams` 可把團隊劇本批次轉成 pack。
- 要發到 Claude Code 生態？`duduclaw expert export <slug> --format claude-plugin` 轉成 plugin 格式。

## 5. 發佈與品質

今天的發佈方式：把 zip 放任何可下載的網址（GitHub Release 最順手），對方 `pack install <url>`；也歡迎到板模畫廊（`distribution/gallery/`）加一頁。集中式 registry（PR 提交＋自動驗證＋簽章）建置中。

品質建議（未來的分級 scorecard 會看這些）：
- [ ] SOUL 有明確「邊界」段
- [ ] `requires` 誠實列出前置條件
- [ ] 附一份 eval 案例（`duduclaw eval-scaffold` 可從 SOUL 起草）——有 eval 的包在分級上會高一級
- [ ] CHANGELOG 式的版本說明（哪版改了什麼）
