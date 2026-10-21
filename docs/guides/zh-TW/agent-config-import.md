# 從其他平台匯入 AI 員工

在別的平台定義好的 AI 員工可以直接帶進 DuDuClaw：豆包夥伴安裝包、OpenClaw 或 Hermes 資料夾、`AGENTS.md`、Claude Code 子代理、GitHub Copilot 自訂代理、Dify 應用匯出檔。人格、工作規則、服務對象的說明、記憶與技能，各自放進 DuDuClaw 真的會讀的位置。

其他語言：[English](../agent-config-import.md)、[日本語](../ja-JP/agent-config-import.md)。

## 兩個入口

**儀表板**：「AI 團隊」頁 →「上傳安裝」。選 `.zip`，或單一的 `.md`／`.yml`／`.yaml` 檔（上限 50 MB）。儀表板只安裝透過這個按鈕上傳的檔案，要匯入伺服器上的資料夾請用終端機指令。DuDuClaw 先掃描，再列出計畫：會建立哪些員工、每個檔案的去處、哪些技能先停用與原因。按「確認匯入」之前什麼都不會寫入，完成後會顯示實際結果。

**終端機**：

```bash
duduclaw pack install ./報告顧問.zip --dry-run   # 只列計畫，不寫入
duduclaw pack install ./報告顧問.zip             # 匯入
duduclaw pack install ./reviewer.agent.md            # 單一檔案也可以
duduclaw pack install ~/.openclaw                     # 資料夾也可以
```

已有同代號的員工時，加 `--rename` 會以 `-imported` 結尾匯入；不加的話，報告列出衝突並略過那位員工。`duduclaw pack install` 就是安裝專家包的同一個指令，沒有另開匯入指令。要連同通道金鑰與排程整個平台搬過來，仍然用 `duduclaw migrate from openclaw|hermes`（見 [migrate-from.md](migrate-from.md)），它現在也照下面同一張表放置檔案。

## 支援的格式

| 格式 | 怎麼辨認 | 每位員工對應 |
|---|---|---|
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
| `USER.md`（Hermes：`memories/USER.md`） | 知識頁「服務對象」 | 核心層，每次對話都會帶入。 |
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

- 員工之後不能改 SOUL 與 INSTRUCTIONS，所以匯入當下是唯一的檢查點。這兩份和其他所有匯入的文字都會經過提示注入檢查；SOUL 與 INSTRUCTIONS 另外再過技能安全掃描。SOUL 沒通過，那位員工就不匯入；操作指令檔沒通過，就不寫入並在報告註明。
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

## 已知限制

- 豆包安裝包沒有公開規格，匯入器照手上唯一的真實樣本實作；遇到不認得的 manifest 欄位與單元類型會列在報告，不會猜。
- Dify 的工作流程圖不會轉換，只取第一個 LLM 節點的系統提示詞，流程請在 DuDuClaw 重建。Dify 知識庫的內容不在匯出檔裡，要重新上傳。
- 規則資料夾（`.cursor/rules`、`.claude/rules`、`.agents/rules`、Windsurf、Cline、Kiro）只會列出，還不會套用。
- 報告每一行都依儀表板語言顯示；「詳細」裡匯入器自己的技術說明（檔名、原平台的原因）仍是繁體中文。
