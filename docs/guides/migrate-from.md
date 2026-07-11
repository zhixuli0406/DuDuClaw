# 從 OpenClaw / Hermes / paperclip 無痛轉移

`duduclaw migrate-from` 讓你用一行指令，把既有的 OpenClaw、Hermes 或 paperclip
設定搬進 DuDuClaw。它預設是**預覽模式**：只印出「會匯入什麼、跳過什麼、為什麼」，
確認無誤後再加 `--apply` 實際寫入。

```bash
# 預覽（不寫任何檔案）
duduclaw migrate-from openclaw

# 確認計畫後實際套用
duduclaw migrate-from openclaw --apply
```

## 指令

```
duduclaw migrate-from <openclaw|hermes|paperclip> [--source <path>] [--apply] [--rename]
```

| 旗標 | 作用 |
|---|---|
| （無） | 預覽轉移計畫，不寫入任何檔案。 |
| `--source <path>` | 指定來源目錄。openclaw/hermes 有預設值；**paperclip 必填**。 |
| `--apply` | 實際執行寫入。 |
| `--rename` | 遇到同名 agent 時，以 `-imported` 後綴匯入，而不是跳過。 |

每一項都會標示狀態：

- `IMPORTED` — 已（或將）匯入。
- `PARTIAL` — 部分匯入或需人工確認（例如非 Claude 模型）。
- `SKIPPED(原因)` — 因故跳過，附原因（來源缺檔、解析失敗、安全阻擋等）。
- `CONFLICT(原因)` — 目標已有值，為保護既有設定而不覆蓋。

整體結果彙整為 `COMPLETE` / `DEGRADED` / `PARTIAL`。套用後，完整報告會寫到
`~/.duduclaw/imported/<platform>/migration-report.md`。所有 token 值一律以
「前 4 後 4」遮罩顯示，不會明文出現在畫面或報告裡。

## 各平台

### OpenClaw（`~/.openclaw`）

```bash
duduclaw migrate-from openclaw            # 預設來源 ~/.openclaw
duduclaw migrate-from openclaw --source /path/to/.openclaw --apply
```

會讀取 `openclaw.json`（JSON5），並匯入：

- **Agents**：`agents.list[]`（或預設單一 `main`），連同各自的 workspace persona
  （`SOUL.md`）與記憶（`MEMORY.md` / `USER.md` / `memory/*.md` 的條列）。
- **通道 token**：`channels.telegram.botToken`、`channels.discord.token`、
  `channels.slack.botToken` + `appToken`（加密寫入 config.toml）。WhatsApp 為
  linked-device，技術上不可轉移 → `SKIPPED`。
- **模型**：`agents.defaults.model.primary`（剝除 `anthropic/` 前綴）。
- **Anthropic API key**：來自 `env` 段與 `~/.openclaw/.env`。其他供應商金鑰 → `SKIPPED`。
- **Cron**：舊版 `cron/jobs.json`（防禦性解析）。新版 SQLite cron schema 未驗證 → `SKIPPED`。
- **Skills**：依 OpenClaw 優先序尋找 `SKILL.md` 資料夾（先掃描再安裝）。

也支援舊名目錄 `~/.moltbot`、`~/.clawdbot`。

### Hermes（`~/.hermes`）

```bash
duduclaw migrate-from hermes --apply
# 轉移非 active 的 profile：
duduclaw migrate-from hermes --source ~/.hermes/profiles/<name> --apply
```

Hermes 是單一 agent 平台，會產生一個 DuDuClaw agent（id `hermes`）。匯入：

- **模型**：`config.yaml` 的 `model.default`。
- **通道 token**（來自 `.env`）：`TELEGRAM_BOT_TOKEN`、`DISCORD_BOT_TOKEN`、
  `SLACK_BOT_TOKEN` + `SLACK_APP_TOKEN`。`EMAIL_*` 通道 v1 尚未支援 → `SKIPPED`。
- **Persona / 記憶**：`SOUL.md`、`memories/MEMORY.md`、`memories/USER.md`。
- **Cron**：`cron/jobs.json`（防禦性解析）。
- 只轉移 **active profile**；其餘 profile 會列為 `SKIPPED`，並提示用 `--source` 逐一轉。

### paperclip — 走官方匯出

paperclip 的資料在內嵌 PostgreSQL，DuDuClaw 不直連資料庫。請先在 paperclip 端匯出：

```bash
paperclipai company export <company-id> --out ./export \
  --include company,agents,projects,issues,tasks,skills

duduclaw migrate-from paperclip --source ./export --apply
```

`--source` 為**必填**（未給時會印出上面的教學）。匯入：

- **Agents**：`agents/<slug>/AGENTS.md` 的 frontmatter（`name/title/reportsTo/skills`）
  → DuDuClaw agent，body → `SOUL.md`。`reportsTo` 直接映射為 `reports_to`（建立時
  依上下級拓撲排序；偵測到環則全部改為無上級並標 `PARTIAL`）。
- **Tasks**：`tasks/<slug>/TASK.md` → Task Board；`recurring` → cron。
- **Skills**：`skills/<slug>/SKILL.md` → agent SKILLS/（先掃描）。
- **COMPANY.md** → 共享 wiki 頁 `shared/wiki/imported/paperclip-company.md`。
- paperclip 官方匯出格式**不含機密**（channel token / API key / DB id），故通道與
  金鑰一律 `SKIPPED`。

## 安全與資料保全

- **Skills 先掃再裝**：每個 `SKILL.md` 都會先通過 duduclaw-security 的 prompt-injection
  掃描器（6 條規則）。命中即 fail-closed：不安裝、標 `SKIPPED(security)`。匯入的 skill
  `skill_auto_activate` 維持 `false`（安全預設）。
- **絕不覆蓋**：既有同名 agent → `SKIPPED`（或加 `--rename`）；config.toml 已有的
  channel token / API key → `CONFLICT`，原值不動。
- **token 加密落地**：channel token 與 API key 以 AES-256-GCM 加密後才寫入 config.toml，
  不會明文入檔。
- **資料不遺失**：v1 不把對話歷史解析進 `sessions.db`，但 `--apply` 會把原始 session /
  對話檔原樣歸檔到 `~/.duduclaw/imported/<platform>/raw/` 供日後查閱。

## v1 非目標（誠實邊界）

1. 對話歷史入庫（僅原樣歸檔）。
2. OpenClaw 新版 SQLite cron / auth-profiles（schema 未驗證）。
3. WhatsApp linked-device 憑證（綁裝置，不可轉移）。
4. Hermes 非 active profile（可用 `--source` 逐一轉）。
5. paperclip 直讀 Postgres（走官方 export）。
6. 外掛記憶後端（Honcho / Mem0 / QMD / LanceDB）。

## 常見問題

**Q：預覽會不會改到東西？**
不會。沒有 `--apply` 時完全不寫入任何檔案，也不會啟動任何通道。

**Q：跑到一半發現有 CONFLICT 怎麼辦？**
CONFLICT 代表目標已有值、為保護你既有的設定而略過。要換成匯入的值，請先手動移除
config.toml 裡的舊值再重跑，或改用 `--rename` 匯入成獨立 agent。

**Q：非 Claude 模型會怎樣？**
會原樣保留成 `[model] preferred` 並標 `PARTIAL`，提示你人工確認要對映到哪個 runtime
（codex / gemini / openai_compat）。DuDuClaw 不會替你猜。
