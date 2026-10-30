# 從其他平台匯入 AI 員工

在別的平台定義好的 AI 員工可以直接帶進 DuDuClaw：豆包夥伴安裝包、OpenClaw 或 Hermes 資料夾、`AGENTS.md`、Claude Code 子代理、GitHub Copilot 自訂代理、Dify 應用匯出檔。人格、工作規則、服務對象的說明、記憶與技能，各自放進 DuDuClaw 真的會讀的位置。

其他語言：[English](../agent-config-import.md)、[日本語](../ja-JP/agent-config-import.md)。

## 兩個入口

**儀表板**：「AI 團隊」頁 →「上傳安裝」。選 `.zip`，或單一的 `.md`／`.yml`／`.yaml` 檔（上限 50 MB）。儀表板只安裝透過這個按鈕上傳的檔案，要匯入伺服器上的資料夾請用終端機指令。DuDuClaw 先掃描，再列出計畫：會建立哪些員工、每個檔案的去處、哪些技能先停用與原因。按「確認匯入」之前什麼都不會寫入，完成後會顯示實際結果。只有管理員能上傳與匯入；其他角色打開這一頁會看到這個說明，上傳按鈕是停用的。

**終端機**：

```bash
duduclaw pack install ./報告顧問.zip --dry-run   # 只列計畫，不寫入
duduclaw pack install ./報告顧問.zip --confirm-profile <preview-digest>
duduclaw pack install ./reviewer.agent.md --dry-run
duduclaw pack install ~/.openclaw --dry-run
```

已有同代號的員工時，加 `--rename` 會以 `-imported` 結尾匯入；不加的話，報告列出衝突並略過那位員工。同一份內容之前已經匯入過時，加 `--replace <代號>` 取代舊版本（員工改過時還要再加 `--accept-edits`），或加 `--as-copy` 另存一份副本，見下面「再次匯入同一份」。`duduclaw pack install` 就是安裝專家包的同一個指令，沒有另開匯入指令。要連同通道金鑰與排程整個平台搬過來，仍然用 `duduclaw migrate from openclaw|hermes`（見 [migrate-from.md](migrate-from.md)），它現在也照下面同一張表放置檔案。

預覽會顯示四個區段全文與輸入安全掃描結果。終端機匯入必須加上預覽印出的 `--confirm-profile` 摘要；來源內容改變，就要重新確認。AI 工作階段不能匯入。原生 `duduclaw:` 區塊只接受可編輯設定；凍結欄位會逐項列出並拒絕，MCP 伺服器一律不匯入。OpenClaw 雖已退役 `TOOLS.md` 和 `HEARTBEAT.md`，舊檔仍可匯入。

從員工頁匯出，或執行 `duduclaw agent export <id> --format duduclaw --out <path>`。其他格式：`openclaw`、`hermes`、`agents-md`、`claude-subagent`。預設不包含服務對象資料，需明確加 `--include-user`。匯出會拒絕疑似秘密，AI 工作階段不能使用匯出指令。詳見 [Agent File 參考](../../spec/agent-file-spec.md)。

## 支援的格式

| 格式 | 怎麼辨認 | 每位員工對應 |
|---|---|---|
| DuDuClaw Agent File | YAML frontmatter 的 `schema: duduclaw.agent/v1`，不依檔名（上限 256 KiB） | 檔案 |
| 豆包夥伴安裝包（詳情頁「…」› 導出夥伴安裝包） | 有 `units` 的 `manifest.json`，或 `agents/<ref>/agent.yml` 加 `persona/` | `agents/<ref>/` |
| OpenClaw 資料夾 | `openclaw.json`（也認 `moltbot.json`、`clawdbot.json`） | `agents.list[]` 或 `agents.entries` 的一項 |
| OpenClaw／SoulSpec 人格工作區 | 最上層有 `SOUL.md` | 資料夾 |
| Hermes 資料夾（`~/.hermes` 或單一 profile） | `config.yaml` 加 `SOUL.md` 或 `memories/` | 資料夾 |
| Dify 應用匯出檔 | 一個含 `app:` 與 `kind: app`（或 `version`）的 `.yml` | 檔案 |
| Claude Code 子代理、Copilot 自訂代理 | `.claude/agents/*.md`、`.github/agents/*.md`、`*.agent.md`、`*.chatmode.md`，或一個 frontmatter 有 `name` 與 `description` 的 `.md` | 檔案 |
| 操作指令檔 | `AGENTS.md`、`CLAUDE.md`、`GEMINI.md`、`AGENT.md`、`.github/copilot-instructions.md` | 資料夾 |

專家包（`expert.toml`）、Claude Code 外掛和單一 Agent Skill 照舊可用，見[專家包](../../features/zh-TW/32-expert-packs.md)。其他內容一律拒絕，並列出上傳裡實際有什麼，不會只匯入一半。

## 每個檔案的去處

| 來源 | 放到 | 說明 |
|---|---|---|
| `SOUL.md`、豆包 `persona/SOUL.md` | `SOUL.md` | 員工只能讀。 |
| `IDENTITY.md` | `IDENTITY.md`，名稱與 emoji 另寫進 `agent.toml` | OpenClaw 的 `- Name:`／`- Emoji:` 行與豆包的「## 簽名」段落都讀得懂。 |
| `AGENTS.md`、`CLAUDE.md`、`GEMINI.md`、子代理本文、Dify `pre_prompt` | `INSTRUCTIONS.md`（新） | 每次對話都放在 SOUL、IDENTITY 後面。員工只能讀。 |
| `USER.md`（Hermes：`memories/USER.md`） | `USER.md` | 上限 4,000 字，以資料區塊帶入，不當作指令；既有知識頁不搬移。 |
| `TOOLS.md` | 知識頁「工具備註」 | 只是說明，不會開放任何工具。 |
| `HEARTBEAT.md` | 知識頁「定期檢查清單」 | 不會自動排程；需要的話到儀表板建立排程。 |
| `BOOT.md`、Dify 開場白 | 知識頁 | 需要時才搜尋。 |
| `MEMORY.md`、`memory/*.md`（Hermes：`memories/MEMORY.md`） | 記憶，一個條列項目（或一段、一個 `§` 項目）一條 | 低可信度（來源標為匯入），並記下來源檔，之後可以整檔撤回。 |
| `skills/<名稱>/SKILL.md` | 員工的技能 | 見下面「技能」。 |
| 子代理裡的 MCP 伺服器 | 不匯入 | 伺服器是 DuDuClaw 會執行或連線的東西，匯入不會自動加上。報告會逐一列出，信得過的再到儀表板自行設定。 |
| `model` | `[model] preferred` | 只接受 Claude 模型代號。其他模型（`doubao-*`、`gpt-*` 等）會在報告註明，改用預設模型。 |
| `tools`／`disallowedTools` | `allowed_tools`／`denied_tools` | 只會收窄。DuDuClaw 不認得的名稱會丟掉並列出。允許清單一律保留 DuDuClaw 自己的工具（`mcp__duduclaw__*`：知識庫、記憶、任務、委派）。來源有限制工具、但名稱一個都對不上時，會關閉執行指令、寫檔與抓網頁，不會變成全部開放。 |
| `BOOTSTRAP.md`、`CLAUDE.local.md` | 略過 | 一次性設定與個人檔案。 |
| 其他檔案 | 保存在 `<home>/imported/<代號>/source/` | 報告會列出。只保存匯入讀過或列出的檔案；金鑰檔、平台設定檔（`openclaw.json`、Hermes `config.yaml`）、對話紀錄與隱藏資料夾不保存。 |

同時有好幾個操作指令檔時，照這個順序取第一個，其餘列為略過：`AGENTS.md` → `CLAUDE.md` → `GEMINI.md` → `AGENT.md` → `.github/copilot-instructions.md`。內容只有 `@AGENTS.md` 這一行的檔案，會直接讀它指向的檔。

員工代號取來源名稱的英數字形式。名稱沒有英數字（例如中文的夥伴名稱）時，代號是「格式-ref」，例如 `doubao-a1`；顯示名稱仍是中文。通道裡用「@顯示名稱」和「@代號」呼叫員工，所以新員工不會用到團隊裡別人已經在用的名稱、呼叫詞或代號：撞到時代號加上 `-imported`、顯示名稱加上「（代號）」，報告裡會註明。

## 技能

每個技能都會整理成合格的 Agent Skill：代號改成小寫英數字與連字號（和資料夾同名），原本的名稱（常是中文）保留在 `metadata.label`。只有中文名稱的技能會得到 `skill-<8 碼>`。豆包匯出常見的壞掉的 frontmatter 會換掉，本文保持原樣。

以下技能會**先停用**，等管理者核准：

- 附有可執行腳本或程式（`.py`、`.sh`、`.js` 等、`#!` 開頭的檔、可執行檔，或圖片與 PDF 以外的二進位內容；`.svg`、`.html`、`.vbs`、`.wsf`、`.command` 也算）；
- 依賴 DuDuClaw 沒有的工具：飛書文件與 API、豆包內建工具（例如「構建網頁應用」）、扣子外掛、Microsoft 365 Graph。

這些技能放在員工資料夾外面等待，匯入時會在儀表板「待辦」送出一筆審核，並記下每個技能的內容指紋；送審後內容被改過的技能不會啟用。核准後在 AI 團隊卡片按「套用」（或執行 `duduclaw expert skills <代號>`），技能才會加到員工身上，已有同名技能時不覆蓋。拒絕或逾期就維持停用。只是零星提到這些工具的技能會照常安裝，報告裡會提醒。

SKILL.md 沒通過內容安全檢查，或任何文字附件沒通過提示注入檢查的技能，完全不安裝。

## 安全

- 人格、服務對象、工作規則與契約都會先顯示全文和輸入安全掃描結果，交由人確認。任何區段未通過掃描，整份設定檔都不會落地。
- 金鑰一律不匯入：`.env`、`auth.json`、`auth-profiles.json` 等檔案不讀也不保存，`openclaw.json` 裡的通道金鑰在這裡不處理，Dify 的機密變數直接丟掉。
- OpenClaw 工作區路徑指到上傳資料夾以外時，不會去讀。
- 壓縮檔：略過 `__MACOSX/`、`._*`、`.DS_Store`、`Thumbs.db`、`desktop.ini`。檔名先當 UTF-8 讀（有標記，或像 macOS 那樣沒標記），中文 Windows 做的壓縮檔再試 GBK 或 Big5。都對不上，或 GBK 與 Big5 一樣可能時，會拒絕上傳並在訊息裡顯示原始位元組，不會寫出亂碼檔名。符號連結不解開。檔名在每種系統上都要代表同一件事，所以含 `:`、結尾是句點或空白、Windows 裝置名稱（`CON`、`NUL`、`COM1` 等），以及只差大小寫的兩個項目，都會拒絕。專家包的大小、項目數與壓縮比上限一樣適用。
- 技能裡的金鑰檔與隱藏檔（`.env`、`token.json`、`*.pem`、`credentials/`、點開頭的檔案）不會複製，並在報告列出。
- AI 員工不能執行匯入：`duduclaw pack install` 與 `duduclaw expert skills` 在員工的工作階段裡會拒絕執行。
- 員工不能用 Write、Edit、MultiEdit、Bash 或 `office_script` 改自己的 `INSTRUCTIONS.md`，和 SOUL.md、CONTRACT.toml 同樣的保護。

## 復原

在 AI 團隊頁移除這次匯入（或 `duduclaw expert remove <代號>`），會刪掉它建立的員工、等待中的技能，以及 `<home>/imported/<代號>/` 的原始檔。匯入的記憶存在共用的記憶庫，用報告「詳細」裡列出的指令撤回（來源代號不含電腦上的路徑）：

```bash
duduclaw memory forget-source --agent <代號> --session "import:agent-import:<import id>/MEMORY.md"
```

## 再次匯入同一份

同一份內容（同一個匯入代號）之前已經匯入過時，儀表板不會直接失敗，會讓你選：

- **取代之前匯入的版本**：舊版本先移到備份，再用同樣的代號與名稱匯入新版本。員工資料夾移到 `<home>/agents/_trash/<代號>_<時間>`（和移除員工用的是同一個地方），技能、知識頁、原始檔與匯入紀錄移到 `<home>/experts/_replaced/<匯入代號>_<時間>/`。會失敗的檢查（格式、上傳的是不是同一份、AI 員工上限）都在搬動之前做完。新版本寫到一半失敗時，它已經寫入的部分移到備份裡的 `failed-new/`，舊版本搬回去，組織資料（主管、部門）也恢復原樣，新版本送出的審核單會撤回。取代成功時，舊版本還在「待辦」等待的技能與 hooks 審核單會一併撤回。匯入程式如果在取代途中被停掉（儀表板匯入超過時間、電腦關機），它留下的取代紀錄（`experts/_replaced/<備份名稱>.journal.json`）會在 gateway 下次啟動或下一次匯入開始前被讀出來，自動把舊版本還原、新版本做到一半的部分移到 `failed-new/`，並在安全稽核紀錄寫一筆 `expert_replace_recovered`。匯入紀錄裡的員工代號，如果資料夾是在那次匯入之後才另外建立的（員工被移除後有人用同一個代號建了新員工），取代時不會搬動它；檔案系統不提供建立時間時（部分 Linux 環境），照舊一律視為那次匯入的員工。被取代掉、新版本沒有再建立的員工，組織資料一併清掉。
- **另存一份副本**：舊版本不動，這次以 `<匯入代號>-2` 這類新代號匯入，代號已有人用的員工改以 `-imported` 結尾。

DuDuClaw 匯入時會記下每位員工每個檔案的內容指紋。之後選「取代」時，如果有員工在匯入後被改過（人格設定、工作規則、知識頁、記憶檔、新增的檔案等），會先列出改了哪些，要你再按一次「我了解，仍要取代」才會動手；後端在沒有這個確認時一律拒絕。`.mcp.json`、`.claude/`、鎖定檔（`*.lock`）、知識庫的索引與紀錄這類由 DuDuClaw 自己維護的檔案不算修改。這個版本之前匯入的員工沒有指紋，改用檔案修改時間判斷，畫面上會註明。

## 匯入失敗時

失敗時，對話框用白話說明原因、建議怎麼處理，並顯示一個錯誤代碼（例如 `import.unrecognized`）。需要協助時把這個代碼提供給我們就好。常見的幾種：

| 錯誤代碼 | 意思 | 怎麼處理 |
|---|---|---|
| `import.unrecognized` | 看不出是哪一種格式 | 對話框會列出壓縮檔最上層有什麼，以及在更深的資料夾找到可以匯入的內容。匯入時最多往下找兩層只包著一個資料夾的外層（`__MACOSX/`、`.DS_Store` 不算），再深就請直接壓縮那個資料夾。 |
| `import.already_imported` | 之前已經匯入過 | 選「取代」或「另存一份副本」。 |
| `import.replace_edited` | 要取代的員工在匯入後被改過 | 看過列出的修改後再確認一次。 |
| `import.nothing_imported` | 沒有任何員工匯入成功 | 對話框逐位列出原因；撞到同代號的員工可以改用「另存一份副本」。 |
| `import.quota_exceeded` | 會超過授權的 AI 員工上限 | 匯入前就會擋下，什麼都不寫入。上限和建立 AI 員工時用的是同一個。 |
| `import.too_large`、`import.zip_bomb`、`import.too_many_entries` | 檔案太大、解壓後異常變大或檔案太多 | 上限是 50 MB（解壓後也算）。 |
| `import.name_encoding_ambiguous`、`import.name_encoding_unknown`、`import.case_duplicate`、`import.unsafe_path` | 檔名有問題 | 用支援 UTF-8 檔名的工具重新壓縮，並改掉只差大小寫或不安全的檔名。 |
| `import.timeout` | 超過 300 秒還沒完成 | 已停止，什麼都沒改。稍後再試或拆成幾個小包。 |
| `import.unknown` | 沒有預期到的問題 | 「詳細」裡有匯入程式的原始訊息，連同代碼一起提供給我們。 |

上傳本身失敗時（例如超過 50 MB、不是 zip、不是管理員），也會顯示對應的說明。

## 已知限制

- 豆包安裝包沒有公開規格，匯入器照手上唯一的真實樣本實作；遇到不認得的 manifest 欄位與單元類型會列在報告，不會猜。
- Dify 的工作流程圖不會轉換，只取第一個 LLM 節點的系統提示詞，流程請在 DuDuClaw 重建。Dify 知識庫的內容不在匯出檔裡，要重新上傳。
- 規則資料夾（`.cursor/rules`、`.claude/rules`、`.agents/rules`、Windsurf、Cline、Kiro）只會列出，還不會套用。
- 報告每一行都依儀表板語言顯示；「詳細」裡匯入器自己的技術說明（檔名、原平台的原因）仍是繁體中文。
