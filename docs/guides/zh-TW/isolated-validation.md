# 使用明確指定的帳號池驗證

[English](../isolated-validation.md) · [日本語](../ja-JP/isolated-validation.md)

即使另開 DuDuClaw 資料目錄，帳號輪替器預設仍可能偵測主機的 Claude 登入狀態，或讀取程序環境中的供應商 API key。若驗證環境只能使用明確設定的認證資料，請在該環境的 `config.toml` 加入：

```toml
[account_loading]
inherit_host_credentials = false
```

預設值為 `true`，設定檔不存在時也沿用預設。設為 `false` 後，所有帳號載入入口共用這項規則：輪替器不執行 `claude auth status`，不使用載入階段的 `ANTHROPIC_API_KEY` 備援，也不在帳號選擇階段使用各供應商的環境變數備援。空帳號池會保持為空，不會在選擇時重新從環境補入帳號。從 `true` 改成 `false` 並重新載入，也會移除先前自動載入的帳號。

明確設定的 `[[accounts]]`、`[api]`、加密認證資料與 `secret://env/...` 引用仍可使用。明確設定的 OAuth profile 也能繼續使用其指定的主機 profile。若要驗證完全沒有帳號的情況，驗證設定檔也需省略這些明確來源。

TOML 格式錯誤、`account_loading` 不是 table、`inherit_host_credentials` 不是布林值，或設定檔發生非「檔案不存在」的讀取錯誤時，載入會失敗，帳號池會清空，環境備援也會關閉。修正設定檔並重新載入後，才能恢復選擇帳號。

這項設定只控制帳號輪替器的偵測與選擇。子程序環境、作業系統鑰匙圈、各 runtime 自有登入機制、檔案存取與網路權限，仍需另外處理；需要這些隔離邊界時，請使用 runtime 支援的容器隔離。空帳號池只能驗證無帳號路徑；供應商執行仍須另有授權且有效的認證資料，以及實際驗收證據。

相關設定：[帳號輪替](../../features/zh-TW/07-account-rotation.md)、[Discovery 設定](../../features/zh-TW/60-discovery.md)。
