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

外來格式走同一個指令安裝:Claude Code plugin(`.claude-plugin/plugin.json`)、單一 Agent Skill(`SKILL.md`),以及其他平台的 AI 員工定義(豆包夥伴安裝包、OpenClaw、Hermes、`AGENTS.md`／`CLAUDE.md`／`GEMINI.md`、Claude Code／Copilot 子代理、Dify 應用匯出檔,見[從其他平台匯入 AI 員工](../../guides/zh-TW/agent-config-import.md))。格式偵測 fail-closed,認不出的佈局會被拒絕並列出實際找到的內容,絕不半吊子匯入。

## 一種格式,一個入口

專家包、付費團隊劇本、職務組合(preset)以前是三種 manifest 方言配三個安裝動詞。現在是一份 schema(`pack.toml`)配一個指令:

```bash
duduclaw pack list                 # 已安裝的包、本機職務組合、內建目錄裡可裝的東西
duduclaw pack inspect <src>        # 正規化後的樣子,並說明這份檔案是哪一代格式
duduclaw pack install <src>        # 依 kind 分流;付費內容只在一處檢查授權
```

`[pack] kind` 決定路線:`preset`(一份職務設定,不帶身分)、`team`(名冊,也就是專家包一直以來的樣子)、`template`(單人產業板模)。`[pack] tier` 是唯一的付費判定——以前有四段程式各自從目錄路徑猜「這是不是付費內容」,現在包自己帶著 tier,只有一個判斷式讀它。讀不出來的 tier 一律當成付費:一個看不懂的欄位絕不該解鎖付費內容。

`duduclaw expert install` 已在 v1.69.0 移除:執行時會印出 `duduclaw pack install` 並以結束碼 2 離開。製作端動詞(`pack`、`publish`、`export`、`convert-teams`、`hooks`、`remove`)與 `expert list`(列出已安裝的紀錄)仍留在 `duduclaw expert` 之下。儀表板的一鍵安裝與 `experts.install` 走到的是同一段程式,因為它們本來就是驅動 CLI。

**`expert.toml`、`team.toml`、`preset.toml` 仍然讀取。** 磁碟上一個字都不改。`expert.toml`、`team.toml` 與產業包目錄結構已棄用，會跟改寫後的付費板模一起在之後的版本移除，目前沒有訂版號。原因是 `pack.toml` 的團隊包與產業包目前還不能安裝：安裝程式只接受 `pack.toml` 的職務 preset。`preset.toml` 是職務 preset 的儲存格式，不在棄用範圍。`duduclaw pack inspect <dir> --emit-canonical` 會印出舊包對應的 `pack.toml`,讓你自己審過再遷移——而不是讓機器去改寫由你負責的內容。

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

生成分成幾個階段，整組團隊不再擠在一次模型回覆裡:

1. **名單**:成員、角色、職責、回報對象。
2. **每位成員各一次呼叫**:這位成員的角色說明(SOUL),必須有固定章節(我是誰、服務範圍、工作流程步驟、至少兩組好回答範例、每條都附原因的紅線、何時轉交真人、語氣);這位成員自己的技能(何時使用、步驟、輸出範本、完整範例);以及從 SOUL 紅線改寫而來的 CONTRACT 規則。每位部門員工至少有一個自己的技能，總機拿到分派技能。
3. **知識庫**:至少四頁共用 wiki:流程 SOP、詞彙、法規與合規注意事項、常見問答。合規頁開頭一律附上「法規名稱、條號與數字需人工核對」的說明，prompt 也禁止模型捏造條號。

第 2、3 階段同時最多跑兩個模型呼叫。每個階段都會檢查 JSON,不合格就把問題回饋給模型重試一次。重試後仍失敗的成員會先用名單資料寫成暫代的角色說明、不附技能，預覽會標出是哪一位(部分完成的草稿);只有名單失敗或所有成員都失敗，整輪才算失敗。儀表板在背景啟動這輪生成，逐階段顯示進度;取消會停止模型呼叫，草稿保留上一輪的內容。prompt 內建一組範例(一份 SOUL、一個技能、一頁 wiki),有快取的產業包時再加上它的 manifest,所以全新安裝的環境也有同樣的品質基準。新生成的成員不再附帶模型寫的 `agent.partial.toml` 設定。成員代號一律以團隊代號開頭(`flowershop-team` 的成員是 `flowershop-…`),呼叫詞由顯示名稱產生(只取文字與數字),不採用模型給的值。

另有一個選項可以讓知識庫階段上網搜尋(Claude CLI 的 `WebSearch`,在模型供應商那端執行)。其他工具一律關閉，包括 `WebFetch`;Direct API 備援沒有任何工具。用到網路搜尋的頁面會加註標示，預覽可以看全文，管理者勾選「已看過」之前不能安裝。CLI 失敗、改由沒有工具的備援撰寫知識庫時，預覽會註明這次沒有上網搜尋。一輪生成預設 25 分鐘後停止(`config.toml` 的 `[experts] generate_timeout_minutes`,5–120),逾時、失敗或取消都保留上一輪。同時最多 3 個生成工作，安裝不佔名額。

**CONTRACT.toml 的紅線同時是回覆過濾器。** `must_not` 會逐字比對每則通道回覆，命中就擋下該則回覆。所以生成團隊與安裝任何專家包時都會先整理規則:移除萬用字元，刪掉少於 10 個字的規則，也刪掉和員工自己該說的話(轉交真人的說法、好回答範例)相同的規則，避免「退款」這種單一關鍵字把所有提到退款的回覆都擋掉。被刪掉的規則會列在安裝結果裡。安裝前請在預覽裡看過每位成員的紅線。

預覽會列出每位成員的技能與紅線、知識庫頁面、部分完成的說明，以及預計安裝時會略過的項目(已有同名的 AI 員工或 wiki 頁面，或內容沒通過安全檢查)。安裝後，儀表板會逐位成員(技能、CONTRACT.toml)、逐頁 wiki 回報實際裝了什麼;已存在的同名頁面不會被覆蓋，改為列在回報裡。

專家包裡只要有成員代號已被使用，安裝時整組改用同一個編號結尾(`-2`、`-3`…;加 `--rename` 時先試 `-imported`),回報關係留在團隊內。顯示名稱或呼叫詞和現有員工相同的成員會改名為「名稱（代號）」,呼叫詞跟著改;預覽會先提醒，安裝結果列出每一筆改名。專家包的技能數包含每位員工自己的技能。

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
| 生成時同時進行的模型呼叫 | 2 |
| 一輪生成的時間上限 | 預設 25 分鐘(`[experts] generate_timeout_minutes`,5–120) |
| 同時進行的生成工作 | 3(安裝不算) |
| 每個生成團隊的知識庫頁數 | 4–10 |
| 每位生成成員的技能數 | 1–3 |
| 草稿存活時間 | 24 h |
| 生成包中的 hooks | 無(fail-closed) |
| 匯出格式 | `claude-plugin`(P0) |
