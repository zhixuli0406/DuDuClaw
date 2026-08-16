# ADR-003:排除在外的通訊通道(Signal / 個人版 WeChat / Viber)

- Status: Accepted
- Date: 2026-07-08
- Deciders: DuDuClaw maintainers

## Context

DuDuClaw 出貨九個通道(Telegram、LINE、Discord、Slack、WhatsApp、Feishu、Google Chat、
Microsoft Teams、WebChat:數字為本 ADR 撰寫當時的計數;WeCom 與 DingTalk 是後來加入的,
使總數增至十一個)。在 2026-07 的通道缺口盤點中,又評估了三個平台,並**刻意排除**。
把這個決策記錄在此,可以防止同樣的選項每季被重新調查一次;更重要的是,防止有人在
不清楚代價的情況下,悄悄上線一個高風險的非官方依賴。

## Decision

目前**不**為 Signal、個人版 WeChat、或 Viber 建置第一方連接器。

### Signal

- 沒有官方 bot API 存在。`signal-cli` 是社群整合路徑。
- 對先前調研的更正:`signal-cli` 使用的是**官方的 `libsignal`** 函式庫(不是逆向工程
  出來的協定),但它驅動的仍然是一個*個人*Signal 帳號,沒有經過官方認可的 bot 平台。
  這帶有速率限制與帳號被停用的風險,而且 Signal 的服務條款是以人類使用為導向。
- 結論:相對於個人帳號橋接所帶的營運風險,投資報酬率偏低。只有在 Signal 發布官方
  bot/business API 時才重新評估。
- Source: https://signal.org/docs/

### WeChat(個人帳號)

- 個人帳號沒有官方 API;非官方橋接經常觸發帳號封鎖。
- **企業版**路徑(WeCom / 企業微信)是一個*獨立*、經過認可的產品,有官方的
  webhook + WebSocket bot 支援,並被列為獨立的(未排除的)backlog 項目,用於兩岸
  中小企業場景。本 ADR 只排除個人帳號變體。

### Viber

- 自 2024-02-05 起,Viber 的 bot/business 訊息服務**僅限商業合約**,最低約每月
  **€100**。
- 對先前調研的更正:核心事實(付費商業門檻)成立;先前引用的「約 15 分鐘重試」這個
  操作細節並不準確,本 ADR 不依賴它。
- 結論:固定的每月門檻,對個人/一人公司這個目標使用者而言不划算,除非有具體需求出現。
- Source: https://developers.viber.com/docs/api/rest-bot-api/

## Consequences

- 這三者被標記為「已評估、已否決」,讓未來的規劃跳過重複盤點。
- 需要接觸 Signal/Viber 使用者的人,應該透過 Matrix 橋接(Matrix 是另一個獨立、
  未排除的候選)或 email 來走,不走第一方連接器。
- 若任何一個排除條件改變(Signal 推出官方 bot API;Viber 出現免費/低價層),
  以一份取代本 ADR 的新 ADR 重新開啟討論。
