# DuDuClaw Pack Registry（WP2.2 —— 待遷出為獨立 repo）

Expert pack 的社群索引。本目錄是 `github.com/zhixuli0406/duduclaw-registry` 的完整種子（👤 建 repo 時整包搬過去即可）。**registry 只存 metadata，不存工件**——zip 放發佈者自己的 GitHub Release（跟官方 MCP Registry 同哲學）。

## 設計決策（對照 DESIGN-ecosystem-expansion §3 A2/A3）

| 決策 | 做法 | 理由 |
|---|---|---|
| 提交模型 | GitHub PR：`index/<slug>.json`（一包一檔） | GitHub 白嫖身份/hosting/討論區；一人公司零審查勞力 |
| 驗證 | CI 全自動（`scripts/validate.mjs` 零依賴）＋publisher==PR author 檢查＋infra 檔 maintainer-only | 綠燈即可合併；人工只看被檢舉 |
| 雙車道信任 | `contains.hooks/skills`＝code lane → **強制 minisign 簽章**＋發佈者需註冊公鑰（`publishers/<user>/minisign.pub`）；純 agents+wiki＝data lane 免簽 | 宣告式資料與可執行程式碼是兩個風險等級（ClawHavoc 教訓） |
| 金鑰模型 | 發佈者自持 minisign key；**key 換綁走 PR＋人工審**（唯一的人工點） | Great Suspender 教訓：所有權移轉即攻擊面 |
| 品質 | `eval_attached` 為分級信號（A4 scorecard 的第一個輸入），不擋上架 | 分級不守門 |
| 不做 | 店內分潤、人工前置審查、TOS 鎖定 | 負面模式清單 |

## 消費端（後續輪次的 CLI 工作）

- `duduclaw expert install registry:<slug>`：抓 raw index JSON → 驗 sha256 →（code lane）驗 minisig → 走既有 install 管線
- `duduclaw expert publish`：從本機 pack 產 entry JSON＋算 sha256＋（選）簽章，印出「fork → 放檔 → PR」三步
- 板模畫廊（`distribution/gallery/`）改為從本 index 生成 pack 頁

## 發佈者流程（上線後的 README 主文）

1. 照 [build-your-own-pack](../../docs/guides/build-your-own-pack.md) 做好 pack，zip 放你的 GitHub Release
2. fork registry → 加 `index/<你的slug>.json`（照 `index/_example.json`；`sha256` 用 release 資產實算值）
3. 含 hooks/skills 的包：先在 `publishers/<你的帳號>/minisign.pub` 註冊公鑰，並附 `.minisig` 簽章 URL
4. 發 PR → CI 綠 → 合併即上架

## 待辦（多輪拆解）

- [x] R1：index 結構＋schema＋零依賴驗證器＋CI（本輪）
- [ ] R2：`expert publish` 指令＋`expert install registry:<slug>` 解析
- [ ] R3：install 端 minisign 驗證（sha256 之上）＋畫廊接 index
- [ ] 👤：建 `duduclaw-registry` repo、開 auto-merge、branch protection
