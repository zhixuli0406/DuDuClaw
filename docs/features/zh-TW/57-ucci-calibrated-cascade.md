# UCCI 校準串接

DuDuClaw 可以使用 Varun Kotte 的 [UCCI: Calibrated Uncertainty for Cost-Optimal
LLM Cascade Routing](https://arxiv.org/abs/2605.18796)（[參考實作](https://github.com/varunkotte6/ucci)）所產生的 router 檔案，在生成之後決定是否升級（escalation）。Rust 依賴固定在特定 commit，router 檔案採用 UCCI 的版本化 JSON 格式。判斷訊號是 UCCI 的平均 top-2 token margin 不確定度，與 DuDuClaw 先前使用的平均 logprob 分數無關。

## 收集與擬合

在 `inference.toml` 啟用既有的 router 與一個 observation 檔案：

```toml
[router]
enabled = true
fast_model = "your-fast-model"
strong_model = "your-strong-model"
ucci_observations = "ucci/observations.jsonl"
ucci_drop_stop_token = true # for vLLM/llama.cpp if logprobs include EOS
ucci_shadow_strong = true    # opt-in: collect Strong answers for returned Fast replies
ucci_shadow_max_inflight = 1 # cap on concurrent background shadow generations (default 1)
```

目前只有 OpenAI-compatible 推論後端能提供所需的 top-2 logprobs。收集時溫度固定為 0。若伺服器沒有為每個內容 token 回傳兩個候選，`u` 為 null，系統不會做出校準決策。

自 2026-09-29 起，該後端不再自帶 HTTP client：`logprobs` / `top_logprobs` 請求欄位與逐 token 回應解析，都改由共用的 `duduclaw-llm` OpenAI-compat provider 負責。本地後端成為其上的薄殼，只保留兩項本地專屬設定（300 秒請求逾時，以及原樣送出的 model id，避免伺服器上的 `qwen/qwen3-4b` 被誤認為 `provider/model` 限定寫法）。UCCI margin 的計算刻意留在 inference 端：共用 provider 負責傳遞訊號，不負責計分，所以 `ucci_fit.py` / `ucci_pair.py` 的輸入格式沒有變動。observation 檔案以明文保存 prompt 與回答，因此是 opt-in。每一列包含串接共用的 `request_id`、唯一的 `id`、`stage`、`u`、回答、模型、延遲，以及空白的人工標註欄位。

對 Fast → Strong 而言，第二個回答由 `ucci_shadow_strong` 提供。Strong 的生成在**背景**執行：Fast 的回覆一經接受就立即送出，不必等第二次模型呼叫，所以開啟收集消耗的是主機吞吐量，使用者感受到的延遲不變。由此帶來的後果是，observation 的某一列可能在它所描述的回覆**之後**才寫入，並行請求的列也可能交錯，配對時請依 `request_id`，不要依檔案順序（`scripts/ucci_pair.py` 已是如此）。每次追加寫入仍持有跨行程的 advisory lock，一列不會被截斷，也不會在行中交錯。若行程在 shadow 仍在生成時結束，只會遺失最後那一列。需要等待的嵌入端可呼叫 `InferenceEngine::flush_shadow_observations()`，等待進行中的工作完成。

shadow 移到背景任務後，它可能與下一個請求自己的前景生成重疊，競爭同一個後端／模型槽位。`ucci_shadow_max_inflight`（預設 `1`）限制同時執行的 shadow 生成數：上限已滿時新的 shadow 會被**略過**，不排隊，也不阻塞前景回覆，略過的次數會被計數並以 `debug!` 記錄。只有在後端確實能同時服務重疊的生成時（例如支援 batching 的伺服器）才調高它。`0` 與 `1` 等價，不代表無上限。

以下指令會產生人工審查用的範本：

```sh
python3 scripts/ucci_pair.py --observations observations.jsonl \
  --stage local_fast --out fast-review.jsonl
```

對 Strong → Cloud，請另外以相同的 `request_id` 與 `stage = "cloud_api"` 收集 Cloud 的回答，再對同一個 helper 傳入 `--cloud-observations cloud.jsonl --stage local_strong`。請人工審查**兩個**回答並填入標註欄位：

```json
{"id":"example-local-fast","stage":"local_fast","u":0.42,"answer":"local answer","large_answer":"strong answer","small_correct":0,"large_correct":1,"label_source":"human","split":"cal"}
```

Fast → Strong 使用 `stage = "local_fast"`，Strong → Cloud 使用 `stage = "local_strong"`。不要把 router 的 `escalated` 決定或 LLM judge 的裁決當作準確度標註；後者可另外保存供稽核。納入範例時，不論目前的閘門是否會升級它們都要納入，只挑被升級的範例會讓擬合產生偏差。校準（calibration）、驗證（validation）與測試（test）的範例必須互不重疊。helper 接受以 `answer` 或 `small_answer` 作為本地回覆。

安裝 `ucci-router`，再以**選定的準確度目標**與**實測的單次呼叫成本**擬合每個階段：

```sh
python3 -m pip install ucci-router==0.1.1
python3 scripts/ucci_fit.py --data reviewed.jsonl --stage local_fast \
  --tau 0.90 --c-small 1 --c-large 3 --out fast-router.json
python3 scripts/ucci_fit.py --data reviewed.jsonl --stage local_strong \
  --tau 0.95 --c-small 3 --c-large 12 --out strong-router.json
```

上面的數值只示範指令形式，目標與成本請依你的工作負載選定。helper 會拒絕非人工標註的資料，以及沒有成對回答的列。它呼叫 UCCI 的 `fit` 時帶上 `--cost-model sequential`，因為 DuDuClaw 是先執行目前的模型，再決定要不要付費呼叫下一個。UCCI 以獨立的校準切分擬合 isotonic map，以驗證切分選定門檻，以測試切分做最終評估。啟用擬合後的檔案之前，請先執行 `ucci evaluate --router fast-router.json --data fast-router.json.reviewed.jsonl --split test` 以及對應的 `ucci report` 指令。helper 會把只含標註的輸入存放在每個 router 旁邊，讓 UCCI 能重現切分並驗證資料摘要。請連同每個檔案保留原始的審查後回答與模型版本。

## 服務

驗證擬合後的檔案之後，設定：

```toml
[router]
enabled = true
ucci_fast_router = "ucci/fast-router.json"
ucci_strong_router = "ucci/strong-router.json"
ucci_observations = "ucci/observations.jsonl"
ucci_drop_stop_token = true
local_tools = false # for a dedicated bare-completion calibration trial
```

相對路徑從 DuDuClaw home 目錄解析。只有檔案有效且其 cost model 為 `sequential` 時，router 才會載入。每個本地階層使用各自擬合的 router。當校準後的錯誤機率**嚴格大於**該階層的門檻時，UCCI 就會升級。對**已設定**的階層而言，檔案遺失、缺少 top-2 訊號或後端不支援，都會升級；沒有 UCCI 檔案的階層則接受其本地回答。在把校準閘門視為已生效之前，請先檢查警告訊息與 observation 列。UCCI 現在是 router 中唯一的校準閘門：舊的 `post_hoc_enabled` / `post_hoc_alpha` / `post_hoc_beta` / `post_hoc_accept_threshold` logistic 設定已於 2026-09-29 移除（`wiki/reports/feature-audit-2026-09-29.md` T3-S7），原因是它們的預設值從未擬合過：在 alpha 4.0 / beta -2.0 / threshold 0.5 下，「機率」只是平均 logprob >= ln 0.5 的固定切點，也沒有任何地方把分數與結果標註一起保存。這四個鍵若留在 `inference.toml` 中會被忽略；沒有 UCCI 檔案的階層直接接受其本地回答。

## 目前的邊界

gateway 的 MCP tool loop 走另一條 provider 路徑，不會經過 `InferenceEngine::route_and_generate`，它的回覆不在這個校準閘門的範圍內。要評估擬合後的閘門，請用 `local_tools = false` 的專用純文字補全（bare-completion）工作負載；需要工具的任務沿用既有的可用工具路徑。`ucci_shadow_strong` 會為本來會直接送出的 Fast 回覆記錄 Strong 的回答，在背景執行，也可能在回覆送達之後才寫入（見上文）。observation 檔案不會執行 Cloud 的 shadow 回答，也不會附上結果標註。Strong → Cloud 的驗證需要另外收集 Cloud 輸出。兩階段串接同樣需要各階段專屬的資料：以所有 Strong 請求訓練出的 Strong → Cloud 擬合，可能和通過 Fast 閘門後才抵達 Strong 的那群請求不同。
