# LINE OA B2C：多客戶官方帳號與點數計費

DuduCloud 可以在同一個 gateway 上託管多個客戶的 LINE 官方帳號。每個官方帳號各自綁定一個 agent 並持有點數餘額；客戶的終端使用者與該 AI 客服 agent 對話，每次回覆都會消耗點數。

## 設定多組官方帳號

`config.toml`：

```toml
[[channels.line.accounts]]
name              = "acme-support"      # label + credit namespace
channel_token_enc = "…"                 # AES-256-GCM (or channel_token plain)
channel_secret_enc = "…"
agent_id          = "acme-agent"
credit_rate       = 2.0                 # points per 1K output tokens; 0 = off

[[channels.line.accounts]]
name              = "beta-shop"
channel_token_enc = "…"
channel_secret_enc = "…"
agent_id          = "beta-agent"
credit_rate       = 1.5
```

舊版單一官方帳號的設定寫法（頂層 `channel_token` / `channel_secret`）依然可用，會解析成一個名為 `default` 的帳號，既有部署不需要任何變更。

## 點數管理（操作者）

點數由操作者核發；計費結算（透過 PayUni 儲值）是另一條由操作者把關的獨立流程。

```bash
duduclaw credit grant acme-support U1234567890 500 --reason "monthly plan"
duduclaw credit balance acme-support U1234567890
duduclaw credit history acme-support U1234567890
```

計費方式：每次回覆消耗 `ceil(output_tokens / 1000 * credit_rate)` 點。當使用者餘額歸零且計費開啟（`credit_rate > 0`）時，回覆會在呼叫 LLM **之前**就被拒絕（fail-closed），並提示使用者儲值。`credit_rate` 設為 0 即關閉該官方帳號的計費。

## 現況

設定模型、點數帳本與操作者 CLI 都已就緒並通過單元測試。剩下的整合步驟，是把共用的 `/webhook/line` endpoint 接上依 LINE `destination` 欄位路由（逐帳號驗簽、不符即 fail-closed），並在每次回覆時做扣點與放行判斷；在這之前，單一官方帳號仍走舊路徑運作。
