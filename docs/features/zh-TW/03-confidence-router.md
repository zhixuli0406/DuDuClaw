# 信心路由器與本地推論引擎

> 智慧模型選擇，節省 80% 以上 API 費用。

---

## 比喻：公司的差旅報銷制度

每間公司都有分級的差旅制度：

- **經濟型**：國內航班、商務旅館。例行出差用。
- **商務型**：好一點的座位、好一點的飯店。重要客戶會議用。
- **頭等艙**：只有執行長見財星 500 大合作夥伴時才用。

沒人會搭頭等艙去參加內部站會。差旅審核員看的是這趟出差的重要性，然後分配適當的等級。

DuDuClaw 的信心路由器就是 LLM 查詢的差旅審核員，評估每個查詢的複雜度，然後將它路由到能勝任的最便宜模型。

---

## 運作方式

### 三層級

| 層級 | 處理者 | 使用時機 | 成本 |
|------|--------|----------|------|
| **LocalFast** | 小型本地模型（例如 7B 參數） | 簡單查詢、問候、事實查找 | 免費（本地運算） |
| **LocalStrong** | 較大本地模型（例如 13B+ 參數） | 中等複雜度、摘要、翻譯 | 免費（本地運算） |
| **CloudAPI** | Claude API | 複雜推理、創意任務、多步驟分析 | 按 token 計費 |

### 信心評分

查詢到達時，路由器用輕量啟發式規則計算信心分數：

```
查詢到達
     |
     v
+-----------------------+
| 計算 token 數量       |  <-- 較短的查詢通常較簡單
| 偵測複雜度關鍵字      |  <-- 「分析」、「比較」、「設計」
|                       |      等關鍵字暗示較高複雜度
| 估算 CJK 比例        |  <-- 中日韓文字有不同的
|                       |      token 密度（約 1.5 字元/token
|                       |      vs 英文約 4 字元/token）
+-----------------------+
     |
     v
信心分數 (0.0 - 1.0)
     |
     +---> > 高閾值  --> LocalFast
     |
     +---> > 低閾值  --> LocalStrong
     |
     +---> <= 低閾值 --> CloudAPI
```

評分完全是規則式的：不需要 LLM 呼叫來決定使用哪個 LLM。閾值和關鍵字列表皆可設定。

### CJK 感知的 Token 估算

對 CJK（中日韓）使用者來說，這是個微妙但重要的細節。英文文字平均約 4 字元/token，但 CJK 文字平均約 1.5 字元/token。一則 100 字元的中文訊息消耗大約 67 個 token，而 100 字元的英文訊息只消耗約 25 個。

路由器在估算查詢複雜度時會考慮這個差異。若缺乏 CJK 感知，系統會系統性地低估中文查詢的複雜度，將它們路由到能力不足的模型。

---

## 路由器背後的推論引擎

有哪些模型、怎麼挑、怎麼裝，請看 **[53-local-models.md](53-local-models.md)**——後端這條線現在歸那一頁。這一節只講路由器需要知道的部分。

### 只有一個出貨的後端

**OpenAI 相容 HTTP** 是唯一有出貨的 `InferenceBackend` 實作。它可以對接任何講 OpenAI chat-completions API 的服務：llama-server、Ollama、llamafile 單檔伺服器、vLLM、SGLang。設定寫在 `inference.toml [openai_compat]`。

本頁原本列出的那些程序內後端，已於 2026-09-29 移除（`wiki/reports/feature-audit-2026-09-29.md` T1-D2/D3、T3-S4/S5）：

- **llama.cpp**（`llama-cpp-2`）——release build 從來沒有編進 `metal`／`cuda`／`vulkan` feature，而且它的 `generate()` 是個回傳「not yet fully implemented」的 stub。
- **mistral.rs**（`mistralrs-core`，ISQ／PagedAttention／Speculative Decoding）——同樣從未編進任何出貨 binary。
- **MLX bridge**（`mlx_lm` Python 子行程）——零呼叫端。本頁描述的「不需 API 呼叫的本地反思」路徑在程式碼裡從來不存在。
- **Exo 分散式叢集**——repo 裡沒有任何範例設定，使用者實際上碰不到；把 `[openai_compat] base_url` 指向 Exo 端點就能連到同一個叢集。

`BackendType::LlamaCpp` 與 `MistralRs` 保留成可解析的設定值，讓舊的 `inference.toml` 仍能載入，但選到它們會回 `BackendUnavailable`，訊息指向 `openai_compat`。

v1.67.1 起寫入時會檢查：

- `inference.update`（儀表板推論頁）只接受 `backend = "openai_compat"` 或空值。空值會刪掉這個鍵，由引擎自己選 `openai_compat`。其他值在寫入任何東西之前就被拒絕，訊息指向 `openai_compat`。有一個例外讓舊檔案仍能儲存：已經存在檔案裡的已移除值原樣送回時照收，所以在你改掉它之前，這一頁仍能儲存其他設定。頁面會把這種值標示為已停止支援。
- `agents.update` 拒絕 `openai_compat` 以外的 `[model.local] backend`（這項檢查在 v1.67.1 之前就有）。v1.68.0 起員工編輯頁完全不再送出 `[model.local] backend`／`context_length`／`gpu_layers`。
- `[model.local] backend` 的預設值、`duduclaw onboard`、`duduclaw wizard` 與內建 agent 範本現在都寫 `openai_compat`（之前寫 `llama_cpp`）。
- 儀表板拿掉了沒有程式讀取的欄位：推論頁的「記憶體上限」（`max_memory_mb`）、生成設定的「GPU Layers」「Context 大小」（`[generation] gpu_layers`／`context_size`）；AI 員工編輯頁的「Context 長度」「GPU Layers」（`[model.local] context_length`／`gpu_layers`）。記憶體、GPU 卸載與 context 大小由外部伺服器自己管。已儲存的值留在檔案裡不動。

v1.68.0 起：

- `inference.update` 存檔成功後會重設 gateway 快取的推論引擎，下一則通道回覆與派工就用新設定（之前約 20 個推論設定要重啟才生效）。頁面會提示引擎已重新載入。
- 推論頁以帶型別的欄位編輯 `[llamafile]`（`enabled`、`dir`、`default_file`、`host`、`port`、`gpu_layers`、`context_size`、`extra_args`），並在「信心路由 (Router)」→「進階」編輯 `[router] local_tools` 與 UCCI 相關鍵（`ucci_fast_router`、`ucci_strong_router`、`ucci_observations`、`ucci_shadow_strong`、`ucci_shadow_max_inflight` 1 到 16、`ucci_drop_stop_token`），以及 `[generation] capture_logprobs`／`capture_top_logprobs`。在頁面上清空 llamafile 欄位不會刪除已存的值，請用「設定檔進階編輯」。
- `max_memory_mb`、`[generation] gpu_layers`／`context_size` 與 `[embedding]` 已從設定結構移除，`inference.update` 收到時忽略。

### InferenceManager 狀態機

管理器維護一條帶自動容錯的優先鏈：

```
優先 1：llamafile（單檔案零安裝）
     |
     v  （不可用？）
優先 2：Direct Backend（程序內的 `InferenceBackend`；今天一個都沒出貨）
     |
     v  （無本地 GPU / 模型太大？）
優先 3：OpenAI 相容伺服器（llama-server、Ollama、vLLM、SGLang…）
     |
     v  （無外部伺服器可用？）
優先 4：Cloud API（Claude——最後防線，永遠可用）
```

每一層有定期健康檢查。當某層變得不健康（當機、記憶體不足、回傳錯誤），管理器自動降級到下一層；恢復時自動升回。

---

## llamafile：零安裝推論

llamafile 值得特別介紹。這是 Mozilla 的專案，將 LLM 模型和推論引擎打包成一個可執行檔。DuDuClaw 以子程序方式管理 llamafile：

```
使用者請求本地推論
     |
     v
llamafile 正在執行嗎？
     |
  +--+--+
  |     |
 是     否
  |     |
  |     v
  |  啟動 llamafile 子程序
  |  等待健康檢查（就緒輪詢）
  |     |
  v     v
將查詢路由到 localhost:{port}
     |
     v
回傳回應
```

管理器處理完整生命週期：啟動、健康監控、停止。llamafile 伺服器在 localhost 上暴露 OpenAI 相容 API，因此路由器像對待其他後端一樣對待它。

結果：跨 macOS、Linux、Windows、FreeBSD 等平台的可攜式零安裝本地推論。

---

## 這為什麼重要

### 成本削減

最直接的好處：不需要 Claude 完整推理能力的查詢不會被送到 Claude。「東京現在幾點？」這類查詢在本地處理時成本為零。每日數千次查詢累積下來，節省 80% 以上。

### 延遲

本地模型的回應時間是毫秒級，而非秒級。對簡單查詢，使用者幾乎即時獲得回應，無需等待雲端往返。

### 隱私

本地處理的查詢永遠不會離開機器。對敏感資料或受法規限制的環境，這是關鍵優勢。

### 韌性

如果雲端 API 當機、限速或緩慢，本地模型讓系統持續運作。多層級容錯確保*永遠*有模型可用來處理查詢。

---

## 透過 MCP 管理模型

推論引擎完全可透過 MCP 工具管理：

| 工具 | 用途 |
|------|------|
| `model_list` | 列出 `~/.duduclaw/models/` 底下的 GGUF 檔案 |
| `model_load` / `model_unload` | 載入/卸載模型生命週期 |
| `inference_status` | 目前載入的模型、硬體、記憶體用量、後端類型 |
| `hardware_info` | GPU 自動偵測、VRAM、RAM、建議設定 |
| `route_query` | 預覽路由決策而不實際生成 |
| `inference_mode` | 目前模式（llamafile / direct / openai-compat / cloud-only） |
| `model_search` | 依 RAM 條件搜尋 HuggingFace + 精選模型庫 |
| `model_download` | 下載到 `~/.duduclaw/models/`，支援斷點續傳與 mirror 容錯 |
| `model_recommend` | 依硬體條件建議適合的模型 |

---

## 與其他系統的互動

- **帳號輪替**：本地推論處理的查詢不消耗任何 API 帳號，延長配額使用期限。
- **CostTelemetry**：追蹤每個查詢由哪個層級處理，使營運人員能調整閾值以達最佳成本/品質平衡。快取效率低於 30% 時，自適應路由會自動偏向本地。
- **演化引擎**：路由器的決策回饋到預測引擎的準確度指標。
- **Multi-Runtime**：信心路由器位在 runtime 層*之下*；它決定模型，而 runtime 決定 CLI 後端。

---

## 總結

不是每個問題都需要最昂貴的答案。信心路由器確保每個查詢獲得能勝任的最便宜模型。多後端引擎則確保永遠有模型可用，從筆電 GPU 到分散式叢集到雲端。
