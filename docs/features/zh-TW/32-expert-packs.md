# 專家包(Expert Packs)

> 一個 zip 裝下一整支 AI 團隊:agent、skill、SOP 與組織定位,經過一條對匯入內容零信任的安全管線安裝。

---

## 這是什麼

專家包是一支團隊的可攜打包:一個或多個以 `reports_to` 階層組織的 agent,加上讓這支團隊第一天就能上工的 skill、共享 wiki SOP 頁面、建議 prompt 與通道提示。原生格式是一個目錄(或 `.zip`、或 `https://…zip` URL),帶一份 `expert.toml` manifest:

```text
<slug>/
├── expert.toml                       # manifest: roster, category, prompts, requires
├── agents/<name>/soul.md             # persona (becomes SOUL.md)
├── agents/<name>/agent.partial.toml  # settings fragment, deep-merged onto the scaffold
├── skills/<name>/SKILL.md            # Agent Skills spec, verbatim
└── wiki/<ns>/*.md                    # shared-wiki SOP / policy pages
```

名冊裡每個成員帶 `name`、`role`、`reports_to`(包內主管)、`department`、`rank`、一個觸發關鍵詞,以及一份 skill 清單。`duduclaw expert pack` 在打包前嚴格驗證(slug 形狀、重複名稱、以 topological sort 檢查 `reports_to` 循環、缺 `soul.md`、SKILL.md frontmatter 的 name 必須等於其目錄名);`install` 則寬鬆,只回報問題,不猜著修。`[expert.requires]` 以 doctor 風格列出環境變數與二進位:缺了會警告,絕不擋下安裝。

兩種外來格式走同一個指令安裝:Claude Code plugin(`.claude-plugin/plugin.json`)與單一 Agent Skill(`SKILL.md`)。格式偵測 fail-closed,認不出的佈局會被拒絕並列出實際找到的內容,絕不半吊子匯入。

## 安裝管線

每次安裝(包括儀表板一鍵安裝與 LLM 生成的草稿)都跑同一套流程:

1. **圍住封存檔。** Zip 解壓有 zip-slip 防護與 50 MB 上限;URL 下載共用同一上限。
2. **先驗證掛載位置。** `--attach-under <agent>`(把包的根 agent 掛在既有主管之下,例如你的 CEO)在寫入任何東西之前先檢查;打錯字會直接中止,什麼都沒裝。
3. **掃描每一個外來物。** 每份匯入的 SOUL/SKILL 文字都降格為 DATA,並同時通過 prompt-injection input guard 與 skill 安全掃描器。任何 block 級發現都會擋下該資產落地;報告會說明原因。
4. **父先子後 scaffold。** Agent 依拓撲順序安裝,`reports_to` 永遠指向真實存在的對象。名稱衝突回報為 conflict,除非以 `--rename` 同意加上 `-imported` 後綴。未知的 role 字串退回 `worker`(壞 role 會讓 agent 在 registry 載入時報廢);非 Claude 的 model id 原樣保留但標記待審,平台永不靜默強制改成單一模型。
5. **合併,不覆蓋。** `agent.partial.toml` 深度合併到 scaffold 出的 `agent.toml`;包宣告的 MCP server 合併進 agent 的 `.mcp.json`,但已接線的 `duduclaw` server 項目永不被覆寫:惡意的包無法劫持工具面。
6. **隔離 hooks。** 見下文。

`--dry-run` 完整渲染安裝計畫,一個位元組都不寫。每個項目都會出現在誠實的報告裡(imported / skipped / conflict / warning)。沒有東西被靜默丟掉。`expert remove <slug>` 只刪安裝紀錄記載為該包建立的東西;先前就存在的資產保留。

## 組織定位:部門 × 職級

名冊成員帶 `department` 時,會寫入 `agent.toml` 的 `[agent] department`,安裝器並建立對應的共享 wiki 部門空間。新進成員立即出現在組織圖與部門頁上。職級(`executive` / `manager` / `staff`)是顯示用 metadata:manifest 未給時由 role 推導(`ceo` → executive;`main` / `front_desk` / `team_leader` / `product_manager` → manager;其餘為 staff)。職級是推導出來的,從不具權威,`reports_to` 樹仍是階層的唯一事實來源。

## 內建目錄

儀表板目錄把 22 套 premium 產業團隊劇本(診所、藥局、會計、法律事務所、電商……)以一鍵安裝的包形式呈現,由冪等的 `expert convert-teams` 管線按需轉換成帶版本的快取。獨立的專家包與團隊並列。項目分成六個分類區(健康、專業、零售、生活、教育、其他),每張卡片顯示其名冊會落入哪些部門,讓 22+ 個包讀起來像一份組織選單,而非一片平鋪的格子。

## LLM 引導的自製流程

沒有適合你生意的包?用描述的。引導流程收一個產業提示、一段自由文字描述(上限 2,000 字元)、團隊規模(1–8)與建議通道。模型輸出**嚴格 JSON 設計**:它從不寫檔案;gateway 把設計實體化成草稿包,用 manifest 驗證器的鏡像做驗證,再顯示預覽。每份草稿最多 5 輪生成/修改;草稿 24 小時後過期。

兩條硬規則讓自製流程安全:生成的包**永遠不得包含 hooks**(prompt 中封鎖並事後驗證,fail-closed);安裝草稿要走上面完整的 CLI 安全管線,LLM 輸出視同外部內容,和陌生人的 zip 一視同仁。

## Hooks:隔離,直到有人點頭

匯入的 hooks 是接進 agent runtime 的任意指令——供應鏈風險。所以它們以**停用**狀態複製進隔離目錄,永不隱式接線。啟用需要明確授權:安裝時給 `--trust-hooks`,或事後在儀表板審批中心經 ApprovalBroker 裁決、再以 `duduclaw expert hooks <slug>` 套用。沒有授權、被拒絕、或 TTL 過期的審批,都讓 hooks 維持停用(fail-closed)。狀態機(`disabled → pending_approval → enabled | disabled`)按包持久化,CLI 與儀表板共用。

## 匯出

`duduclaw expert export <slug> --format claude-plugin` 把已安裝的包轉回 Claude Code plugin:`.claude-plugin/plugin.json`(DuDuClaw 專屬欄位放在 `x-duduclaw` key 之下,Claude Code 會忽略)、每個 agent 一份 `agents/<id>.md`(frontmatter 加 SOUL 本文),以及各 agent 的非 duduclaw MCP server 聚合到 plugin 層級。在這裡組的團隊,不會被鎖在這裡。

## 限制

| 項目 | 限制 |
|---|---|
| 封存檔大小(zip / 下載) | 50 MB |
| 生成團隊規模 | 1–8 個 agent |
| 每份草稿生成輪數 | 5 |
| 草稿存活時間 | 24 h |
| 生成包中的 hooks | 無(fail-closed) |
| 匯出格式 | `claude-plugin`(P0) |
