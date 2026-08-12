# TODO: Telegram 引用/回覆訊息內容遺失（reply_to_message 未解析）

> 狀態：**Resolved（2026-08-12，主體修復完成，欠活體驗證）** · 類型：Bug（平台，影響所有 Telegram agent）· 優先：High
> 發現於：2026-08-12，LWM 交易實驗中使用者「回覆某則 bot 訊息並追問」時，agent 完全看不到被引用的內容。
>
> **修復摘要（2026-08-12）**：
> - `TgMessage` 補宣告 `reply_to_message: Option<Box<TgMessage>>` 與 `forward_origin`；
>   引用內容（text → caption → 媒體占位）以統一格式前置到 agent 輸入
>   （`channel_format::format_quoted_context`，CJK 安全截斷 2000 bytes）。
> - 被引用者是 bot 自己時標示「你（bot）先前發送的訊息」（本 bug 的主要觸發情境）。
> - 轉發訊息標注原始來源（user / hidden_user / chat / channel 四型）。
> - 加修同類：群組 mention-only 模式下「回覆 bot 訊息」現在視同 @ 提及（Telegram + Discord）。
> - 延伸掃描確認**全部 11 個通道**接收路徑都有同型缺口；payload 內已含引用內容的
>   Discord / Slack / Teams / WhatsApp 已一併修復，其餘見
>   [TODO-channel-quote-context-remaining.md](TODO-channel-quote-context-remaining.md)。
> - 單元測試：`telegram::reply_context_tests`（5 條）等 14 條新測試全綠；
>   gateway lib 全套 4673 tests 零回歸。
> - **未完成**：真機活體驗證（回覆 bot 通知並追問）；引用照片下載（原修法第 3 步選配）→ 移至 remaining TODO。

## 一句話

使用者在 Telegram **回覆（引用）一則訊息**時，被引用訊息的內容沒有被帶進 agent 的輸入；agent 只收到使用者新打的那句話，於是誠實地回「我沒收到任何轉貼內容」。

## 症狀

1. 使用者長按一則訊息 → 回覆 → 打字送出。Telegram 畫面上該則被引用訊息清楚顯示在使用者訊息上方。
2. Agent 回覆時表示看不到被引用的那段（例：「『這裡』指的那段文字沒有跟著傳過來」）。
3. 對使用者而言像是 agent 裝傻或失憶，實際上是輸入根本沒帶到引用內容。

## 根因

Telegram Bot API 在使用者回覆訊息時，`update.message.reply_to_message` 會帶完整的被引用訊息物件。但：

- 接收用的 `TgMessage` struct 沒有宣告 `reply_to_message` 欄位
  （[`crates/duduclaw-gateway/src/telegram.rs:116-136`](../../crates/duduclaw-gateway/src/telegram.rs#L116-L136)）。
  `#[derive(Deserialize)]` 對未宣告欄位是**靜默丟棄**，引用內容就此消失。
- 組 agent 輸入 `input_text` 的區段只從 `msg.text` / 語音轉錄 / `msg.caption` 取，
  從不參照任何被引用訊息
  （[`crates/duduclaw-gateway/src/telegram.rs:600-635`](../../crates/duduclaw-gateway/src/telegram.rs#L600-L635)）。

註：`telegram.rs` 內既有的 `reply_to_message_id` 只用於**送出**方向（bot 回覆某訊息），與**接收**解析無關，不要混淆。

## 影響範圍

- 全平台所有 Telegram agent，不限特定專案。
- 任何「回覆某則訊息再追問」的自然用法都會靜默掉 context——使用者以為給了背景，agent 其實沒收到。
- 相關但獨立的缺口：**轉發訊息**（`forward_origin` / `forward_from`）同樣未解析；轉貼進來的內容也會遺失（見下方「延伸」）。

## 修法

1. `TgMessage` 加欄位（遞迴，需 `Box`）：
   ```rust
   reply_to_message: Option<Box<TgMessage>>,
   ```
2. 在 `input_text` 決定之後、送進 agent 之前，若 `msg.reply_to_message` 存在，取其 `text`（無則 `caption`）前置成引用區塊，例如：
   ```
   〔引用訊息〕<被引用訊息的 text 或 caption>
   <使用者新打的話>
   ```
   - 引用文字做長度上限截斷（用 `duduclaw_core::truncate_bytes`，CJK 安全，勿以 byte index 切）。
   - 被引用者若是 bot 自己發的訊息也照樣帶（使用者常引用 bot 的通知來追問，正是本 bug 的觸發情境）。
3. （選配，第二步）被引用訊息若帶 `photo`，下載其最大尺寸縮圖，比照現有 photo 附件流程落地到 agent 附件目錄，讓「引用一張圖問問題」也成立。

## 延伸（同類一起掃）

- **轉發訊息**：`TgMessage` 也沒有 `forward_origin` / `forward_from` / `forward_date`。若要支援「轉貼一段對話給 agent 判讀」，需一併補上並在 `input_text` 標注來源。列為同一類問題，避免只修一半。
- 檢查 ACP 側 [`crates/duduclaw-cli/src/acp/message_send.rs`](../../crates/duduclaw-cli/src/acp/message_send.rs) 是否有對稱缺口。

## 測試計畫

- [ ] 單元：`TgUpdate` 含 `reply_to_message` 的 JSON 能反序列化，且 `input_text` 前置了引用區塊。
- [ ] 單元：無 `reply_to_message` 時行為與現況 byte-identical（不回歸）。
- [ ] 單元：引用訊息含 CJK/emoji，截斷不 panic（char boundary 安全）。
- [ ] 活體：真機以個人帳號回覆 bot 一則通知並追問，確認 agent 覆述得出被引用內容。

## 文件同步（修好時一起）

- `CHANGELOG.md` → `Fixed`：Telegram 回覆/引用訊息內容現已帶入 agent context。
- 若補了轉發支援 → `Added`。

## 部署備註

修正在 gateway（`duduclaw-pro` binary）。要在運行中的服務（含 LWM 實驗容器）生效需重建 binary + 重烤 image + 重啟。**交易時段（09:00–13:30）不重啟實驗容器**，收盤後再隨其他變更一起部署。純程式碼修正與 compile 驗證可隨時進行。
