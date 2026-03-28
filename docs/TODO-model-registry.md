# TODO: Model Registry — 策展清單 + 動態驗證 + 自動下載

> 智慧模型推薦與下載系統，取代手動放置 GGUF 檔案
> Created: 2026-03-28

## 1. 策展清單 (Curated Registry)

### 1.1 遠端策展清單格式
- [ ] 定義 `model-registry.json` 結構：id, repo, filename, size_bytes, quant, params, languages, tags, min_ram_mb, description
- [ ] 託管於 GitHub（duduclaw-skills repo 或獨立 repo），可獨立於 binary 更新
- [ ] 內建 fallback 清單（硬編碼在 binary 中），網路不通時使用
- [ ] 清單分類：recommended（官方驗證）、community（搜尋結果）

### 1.2 可信上傳者白名單
- [ ] 白名單 HF org/user：Qwen, google, meta-llama, microsoft, bartowski, mlx-community, TheBloke
- [ ] 搜尋結果中標記 [推薦] vs [社群]，社群結果顯示警告

## 2. HuggingFace API 整合

### 2.1 搜尋 API client
- [ ] `model_registry/hf_api.rs` — HF API client (`https://huggingface.co/api/models`)
- [ ] 搜尋參數：filter=gguf, sort=downloads, limit=20
- [ ] 解析回應：modelId, downloads, tags, siblings (files)
- [ ] 從 siblings 過濾 .gguf 檔案，取得 size

### 2.2 智慧篩選
- [ ] 根據 `detect_hardware()` 的 RAM 自動篩選合適大小的模型
- [ ] 根據量化等級排序：Q4_K_M > Q4_K_S > Q5_K_M > Q8_0（品質/大小平衡）
- [ ] 語言偏好：偵測系統 locale，優先推薦支援該語言的模型
- [ ] 去重：同一模型不同量化只顯示最推薦的一個

### 2.3 速率限制與快取
- [ ] 搜尋結果快取 24h（`~/.duduclaw/cache/model-registry.json`）
- [ ] 未認證 API 限流處理（429 → 使用快取或 fallback 清單）
- [ ] 支援 HF_TOKEN 環境變數提高限額

## 3. 下載引擎

### 3.1 HTTP 下載器
- [ ] `model_registry/downloader.rs` — async 下載器
- [ ] 進度條（已下載/總大小、速度、ETA）
- [ ] HTTP Range header 斷點續傳（.gguf.partial 暫存）
- [ ] SHA256 校驗（若 HF 提供）
- [ ] 下載完成後自動移除 .partial 檔案

### 3.2 中國鏡像支援
- [ ] 偵測 HF CDN 連通性（timeout 5s）
- [ ] 自動切換 hf-mirror.com 鏡像
- [ ] 鏡像 URL 可在 inference.toml 中覆蓋

## 4. Onboard 整合

### 4.1 模型選擇 UI
- [ ] 先顯示策展推薦（根據硬體篩選後的 top 3-5）
- [ ] 「搜尋更多模型...」選項 → 進入 HF 搜尋互動
- [ ] 「我已有模型」選項 → 掃描 ~/.duduclaw/models/
- [ ] 「稍後設定」選項 → 跳過，生成空的 inference.toml
- [ ] 每個模型顯示：名稱、大小、用途、是否適合當前硬體

### 4.2 下載流程
- [ ] 選擇模型後確認下載（顯示大小和預估時間）
- [ ] 下載進度條（終端機內即時更新）
- [ ] 下載失敗：提供手動下載 URL + 路徑指引
- [ ] 下載成功：自動寫入 inference.toml default_model

## 5. MCP 工具 + CLI 子命令

### 5.1 CLI 子命令
- [ ] `duduclaw model search <query>` — 搜尋 HF 模型
- [ ] `duduclaw model download <model_id>` — 下載指定模型
- [ ] `duduclaw model list` — 列出本地已下載模型
- [ ] `duduclaw model recommend` — 根據硬體推薦模型

### 5.2 MCP 工具
- [ ] `model_search` — 搜尋可用模型（策展 + HF）
- [ ] `model_download` — 下載模型到 ~/.duduclaw/models/
- [ ] `model_recommend` — 硬體感知推薦

## 6. 測試與文件

- [ ] HF API mock 測試（不依賴網路）
- [ ] 策展清單解析測試
- [ ] 下載器斷點續傳測試
- [ ] 硬體篩選邏輯測試
- [ ] inference.toml.example 更新
- [ ] CLAUDE.md 架構更新
