# TODO: 其餘通道的引用/回覆上下文缺口（全通道掃描結果）

> 狀態：Open · 類型：功能缺口（平台）· 優先：Medium
> 來源：2026-08-12 修 [TODO-telegram-reply-context.md](TODO-telegram-reply-context.md) 時的全通道同類掃描。
> 已修（payload 內含引用內容、零額外 API 呼叫）：Telegram / Discord / Slack（分享訊息）/ Teams（blockquote）/ WhatsApp（標註）。
> 本文件追蹤剩餘項——共同點是**需要額外 API 往返、平台根本不給內容、或需要前端配合**，
> 不適合和零成本修復混在同一批。

## 統一格式（已存在，直接複用）

`channel_format::format_quoted_context(who, excerpt)` + `QUOTED_SELF_LABEL`（bot 自己的訊息）。
所有後續修復一律走這個 helper，勿再各自發明格式。

## 剩餘缺口清單

### 需額外 API 往返才拿得到內容（P2）

| 通道 | 平台欄位 | 現況 | 修法 |
|------|----------|------|------|
| LINE | `message.quotedMessageId` / `quoteToken` | struct 未宣告 | 平台**無「以 id 取訊息」API**——要支援引用需自建近期訊息快取（session 歷史內已有 bot 側訊息，可先比對）；`quoteToken` 順帶補上可解鎖「回覆特定訊息」的送出能力 |
| Feishu | `message.parent_id` / `root_id` / `thread_id` | 只收 `content`，未宣告 | `GET /im/v1/messages/{id}` 拉原文；注意租戶權限 scope |
| Google Chat | `message.quotedMessageMetadata` | 只用 `thread.name` 掛回覆 | `spaces.messages.get` 拉原文；同時評估 thread 歷史（`spaces.messages.list`）要不要抓 |
| Slack（thread 深度） | `conversations.replies` | thread_ts 只當 session key；thread 內歷史靠 session 記憶 | 使用者在 bot 未參與過的舊 thread 中 @bot 時看不到前文——評估首次進入 thread 時抓 N 則歷史 |

### 平台不提供引用資訊（記錄在案，無法修）

- **WeCom（企業微信）**：被動訊息 XML 不含引用內容。
- **DingTalk（釘釘）**：機器人 callback 不帶引用原文。

### 需自訂 schema / 前端配合（P3）

- **WebChat**：`ChatMessage::UserMessage` 無 reply/quote 概念——需擴充 WS schema + dashboard 前端 UI（長按引用）。

### 其他同族

- **Email**：`InboundEmail` 未解析 `In-Reply-To` / `References` header，郵件串聯完全斷裂（email.rs:64）。
- **Telegram 引用照片**（原修法第 3 步選配）：`reply_to_message.photo` 目前給文字占位；可比照現有 photo 附件流程下載最大尺寸，讓「引用一張圖問問題」成立。
- **ACP `message_send.rs`**：`taskId` 多輪延續回 `-32004 Unsupported` 是刻意設計限制（非缺漏）；若日後開 A2A 多輪再一併考慮引用語意。

## 測試計畫（實作時逐通道比照 Telegram）

- [ ] 單元：帶引用欄位的 payload 能反序列化且 input 前置引用區塊。
- [ ] 單元：無引用時行為與現況 byte-identical。
- [ ] 活體：真機引用 bot 訊息追問，agent 覆述得出被引用內容。

## 文件同步（實作時）

- `CHANGELOG.md` → `Added`/`Fixed` 對應條目；本檔逐項打勾，全清後改狀態 Resolved。
