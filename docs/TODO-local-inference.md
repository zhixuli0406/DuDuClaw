# TODO: Local Inference Integration

> DuDuClaw 本地推理引擎整合 — 四階段實施計畫
> Created: 2026-03-28

## Phase 1: llama.cpp 多後端嵌入 (Week 1-2)

### 1.1 建立 duduclaw-inference crate 骨架
- [ ] `crates/duduclaw-inference/Cargo.toml` — feature flags: `metal`, `cuda`, `vulkan`, `cpu`
- [ ] `crates/duduclaw-inference/src/lib.rs` — 模組結構
- [ ] 加入 workspace `Cargo.toml` members
- [ ] workspace dependencies 加入 `llama-cpp-2` crate

### 1.2 定義 InferenceBackend trait 與核心型別
- [ ] `src/backend.rs` — `InferenceBackend` async trait (generate, generate_stream, load_model, unload_model)
- [ ] `src/types.rs` — `InferenceRequest`, `InferenceResponse`, `ModelInfo`, `GenerationParams`, `BackendType`
- [ ] `src/error.rs` — `InferenceError` enum (ModelNotFound, BackendUnavailable, OutOfMemory, GenerationFailed)
- [ ] `src/config.rs` — `InferenceConfig` (從 `~/.duduclaw/inference.toml` 讀取)

### 1.3 實作 LlamaCppBackend
- [ ] `src/llama_cpp.rs` — 封裝 `llama-cpp-2` crate
- [ ] 實作 `InferenceBackend` trait
- [ ] 支援 GGUF 模型載入 (mmap)
- [ ] 支援 streaming token generation (tokio channel)
- [ ] 支援 system prompt + user prompt 格式化 (chat template)
- [ ] 支援 generation params (temperature, top_p, max_tokens, stop sequences)
- [ ] KV-cache 管理 (自動清理)

### 1.4 模型管理器
- [ ] `src/model_manager.rs` — ModelManager struct
- [ ] GGUF 模型目錄掃描 (`~/.duduclaw/models/*.gguf`)
- [ ] 模型 metadata 讀取 (architecture, params, quant type)
- [ ] 模型載入/卸載 with reference counting
- [ ] 並行請求共享已載入模型 (Arc<RwLock>)

### 1.5 硬體偵測與後端自動選擇
- [ ] `src/hardware.rs` — detect_hardware() -> HardwareInfo
- [ ] Apple Silicon 偵測 (sysctl / system_profiler)
- [ ] NVIDIA GPU 偵測 (nvidia-smi)
- [ ] AMD GPU 偵測 (rocm-smi / vulkaninfo)
- [ ] 可用記憶體偵測 (RAM + VRAM)
- [ ] 根據硬體自動選擇最佳後端 + 推薦模型大小

### 1.6 擴展 AccountRotator 加入 LocalInference
- [ ] `AuthMethod` enum 加入 `Local` variant
- [ ] `Account` struct 支援本地模型配置 (model_path, backend_type)
- [ ] `select()` 邏輯：LeastCost 策略時優先選 Local
- [ ] cost = 0 for local inference
- [ ] health check: 模型是否已載入

### 1.7 修改 claude_runner.rs 整合本地推理
- [ ] `call_with_rotation()` 偵測 `AuthMethod::Local` 分支
- [ ] Local 分支呼叫 `InferenceBackend::generate()` 而非 `claude` CLI
- [ ] 回傳格式與 Claude CLI 相容 (text response)
- [ ] 錯誤處理：本地推理失敗時 fallback 到 API
- [ ] 靜態 `InferenceEngine` 實例 (OnceLock + lazy init)

### 1.8 擴展 ModelConfig
- [ ] `ModelConfig` 加入 `local_model: Option<LocalModelConfig>`
- [ ] `LocalModelConfig` struct: model_path, backend, quant_type, context_length, gpu_layers
- [ ] agent.toml 範例更新

### 1.9 MCP Tools
- [ ] `model_list` — 列出可用本地模型
- [ ] `model_info` — 查看模型詳細資訊
- [ ] `model_load` / `model_unload` — 手動載入/卸載
- [ ] `inference_status` — 推理引擎狀態 (已載入模型、記憶體使用)
- [ ] `hardware_info` — 硬體資訊

### 1.10 編譯驗證與測試
- [ ] `cargo build --features metal` 在 Apple Silicon 編譯通過
- [ ] 基本推理測試 (載入 GGUF → generate → 驗證輸出)
- [ ] AccountRotator 整合測試 (Local fallback 邏輯)
- [ ] 更新 CLAUDE.md 架構描述

---

## Phase 2: mistral.rs 原生引擎 + Speculative Decoding (Week 3-4)

### 2.1 加入 mistral.rs 後端
- [ ] `src/mistral_rs.rs` — 封裝 `mistralrs-core` crate
- [ ] ISQ (In-Situ Quantization) 支援：safetensors → 即時量化
- [ ] PagedAttention 啟用
- [ ] Continuous Batching 支援 (多 agent 共享模型)
- [ ] feature flag: `mistralrs`

### 2.2 Speculative Decoding
- [ ] EAGLE-2 draft head 配置
- [ ] Self-Speculative (LayerSkip) 配置
- [ ] `inference.toml` 加入 speculative decoding 區段
- [ ] 速度基準測試與回報

### 2.3 Confidence Router
- [ ] `src/router.rs` — ConfidenceRouter struct
- [ ] 查詢複雜度評估 (token count, keyword heuristics)
- [ ] 三層路由：LocalFast → LocalStrong → CloudAPI
- [ ] 路由決策日誌 (tracing)
- [ ] 可配置閾值 (`inference.toml`)

### 2.4 自動調校
- [ ] `mistralrs tune` 等效邏輯整合
- [ ] 首次啟動時自動 benchmark
- [ ] 結果寫入 `~/.duduclaw/inference_config_cache.toml`

---

## Phase 3: MLX Evolution + Exo 集群 + llamafile (Week 5-6)

### 3.1 MLX Evolution 本地化 (Apple Only)
- [ ] Evolution Engine 的 Micro/Meso reflection 改用本地模型
- [ ] Python subprocess 呼叫 `mlx_lm.generate()`
- [ ] LoRA 適配器支援 (agent 個性化)
- [ ] Feature flag: `mlx` (compile-time Apple detection)

### 3.2 Apple FoundationModels FFI (macOS 26+)
- [ ] `src/apple_fm.rs` — `objc2` FFI 呼叫 FoundationModels
- [ ] Tier 0 路由：意圖分類、安全掃描走 Apple FM
- [ ] Runtime 版本偵測 (macOS 26+ guard)

### 3.3 Exo P2P 集群
- [ ] `src/exo.rs` — Exo cluster HTTP client
- [ ] 集群發現 (mDNS or 手動 `cluster.toml`)
- [ ] 模型分配策略 (根據節點記憶體)
- [ ] 健康檢查與自動 failover

### 3.4 llamafile Fallback
- [ ] `src/llamafile.rs` — llamafile subprocess 管理
- [ ] 自動啟動/停止 llamafile server
- [ ] OpenAI-compatible API client
- [ ] 跨平台零安裝分發 (`~/.duduclaw/llamafiles/`)

### 3.5 模式自動切換
- [ ] InferenceManager: Exo → llamafile → llama-cpp → Claude API
- [ ] 基於硬體/網路/模型大小的自動決策
- [ ] 狀態機 + 健康監控

---

## Phase 4: Token/KV 壓縮整合 (Week 7-8)

### 4.1 LTSC Meta-Token (Rust 原生)
- [ ] `src/compression/meta_token.rs` — 重複子序列偵測
- [ ] compress / decompress API
- [ ] 整合 `bus_queue.jsonl` IPC 壓縮
- [ ] 整合 session history 壓縮

### 4.2 LLMLingua-2 (Python subprocess)
- [ ] Python wrapper: `pip install llmlingua`
- [ ] bridge function: compress_prompt(text, ratio) → compressed_text
- [ ] Session auto-compression 前置處理
- [ ] CJK multilingual 支援驗證

### 4.3 KV-Cache 壓縮
- [ ] TurboQuant+ PolarQuant 研究/整合評估
- [ ] StreamingLLM attention sink 模式
- [ ] 長對話記憶體監控與自動壓縮觸發

### 4.4 整合測試與文件
- [ ] 全平台交叉編譯測試 (macOS, Linux)
- [ ] 效能基準測試報告
- [ ] CLAUDE.md 架構更新
- [ ] 使用者文件 (inference.toml 範例)
