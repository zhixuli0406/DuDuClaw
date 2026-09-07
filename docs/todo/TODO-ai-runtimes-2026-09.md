# TODO — 全 runtime 開箱即用、本地模型、微調介面（2026-09）

> 狀態：第一輪實作完成、QEMU 走查完成（2026-09-06，見 §6）。決策已由維護者拍板：**1B、2 全部、3A、4C**（見 §1）。
> 本文是各工作單元（WP）的共同規格；每個 WP 的完成定義（DoD）以本文為準。

## 1. 決策

| # | 題目 | 決定 |
|---|---|---|
| 1 | 訂閱帳號 OAuth 登入 | **B**：允許各家 CLI 的訂閱登入（Claude Pro/Max、ChatGPT、Google、Kimi、Copilot、Cursor、Grok、Kiro…），但在登入前**明示該廠商條款風險**（Anthropic 與 Google 自 2026-03 起伺服器端封鎖第三方產品使用消費者訂閱 token，已有帳號被停權；OpenAI 政策不明），使用者勾選「我了解風險」才繼續。API key 路徑一律提供且為預設建議。 |
| 2 | 首版內建 runtime 範圍 | **全部**：Node 系（Claude Code、Codex、Gemini CLI、Qwen Code、Kimi Code、GitHub Copilot CLI）＋二進位系（Grok Build、Kiro CLI、Cursor `agent`、OpenCode）＋Python 系（Mistral Vibe）。root A/B 槽由 7168 MiB 升至 **8192 MiB**，`os_update.rs::MAX_ROOT_BYTES` 同步升至 9 GiB。 |
| 3 | 本地模型 | **A**：映像內建 llama.cpp `llama-server`（MIT、Vulkan）；模型權重不進映像，OOBE／dashboard 一鍵從 Hugging Face 下載精選 GGUF 到 `/data/duduclaw/models`。 |
| 4 | 微調／後訓練 | **C**：(a) dashboard「微調與後訓練」頁——從對話／審批紀錄整理資料集 → 送雲端 GPU 或使用者自己的 GPU 主機跑 LoRA／DPO → 匯回 GGUF；(b) LLaMA-Factory LlamaBoard 以 `compat.d` 選裝 runner（docker）提供給有獨顯的進階使用者。 |

## 2. 現況（2026-09-05 調研，為規格前提）

- 平台已有 `RuntimeType {Claude, Codex, Gemini, Antigravity, Grok, OpenAiCompat}`、`runtime/*.rs`（`AgentRuntime` trait）、`cli_auth.rs`（各家 CLI 登入 PTY 流程）、`runtime_install.rs`（安裝白名單）、`runtime_models.rs`（模型探索）、`auth_device.rs`（Copilot／Qwen 裝置碼登入代理）。偵測（`handle_runtime_detect`）、安裝、模型探索是三份手寫清單；`duduclaw-cli-runtime::CliKind` 少 `Grok`。
- `accounts.add` 不接受 `provider`，金鑰欄位寫死 `anthropic_api_key`；dashboard `AddAccountDialog` 沒有 provider 選單；OOBE「AI Runtime 授權」只收 Anthropic 金鑰。
- 本地推理：`duduclaw-inference` 的 llama.cpp／mistral.rs 未編進出貨 binary（feature 關閉、llama.cpp 核心是 stub）；可用路徑只有 `openai_compat`（外部伺服器）與 `llamafile` 子行程；`LocalModelsPage.tsx` 已存在。
- OS：meta-oe 有 `nodejs_22.23.2`（含 `nodejs-npm`）但映像沒裝；大型 vendor blob 走 `duduclaw-flatpak-offline-repo` 的 `file://*.tar.zst` 模式；gateway 以 root 執行、`ProtectHome=read-only`、真 `$HOME=/root`，各家 CLI 的 OAuth token 寫在真 `$HOME` → 需 `Environment=HOME=/data/duduclaw`；`which_cli` 候選路徑沒有 `/usr/bin`。
- 市面：Claude Code npm 套件已標記棄用（改原生安裝器，仍可用）；Qwen 免費 OAuth 2026-04-15 停用（改 ModelStudio／API key）；Amazon Q CLI 已由 Kiro CLI 取代；Cursor 提供 SDK、Copilot CLI 支援 ACP，兩者明確允許第三方嵌入。
- 微調：2026 年的 LLaMA-Factory／Unsloth／Axolotl／h2o 全部假設 CUDA／ROCm；目標機種（N305、8845HS 內顯）無可用訓練路徑；llama.cpp finetune 不穩定。

## 3. 工作單元

### WP-A 帳號與 provider（主 repo，gateway＋dashboard）
- `accounts.add` 接受 `provider`（`duduclaw_core::provider_env::KNOWN_PROVIDER_IDS` 之一）與 `type = api_key|oauth`；`build_account_entry` 依 provider 寫 `api_key_enc`（Anthropic 維持 `anthropic_api_key_enc` 相容）；`accounts.list` 回 `provider`。
- dashboard `AddAccountDialog`：provider 選單（含每家的金鑰格式提示與取得金鑰連結）；OAuth／訂閱登入卡片加入 §1-1 的風險告知與勾選（i18n 三語）。
- DoD：`cargo test -p duduclaw-gateway accounts` 綠；web `npm run build` 綠；`docs/features/13-multi-runtime.md` 三語更新。

### WP-B runtime 註冊表與新 runtime（主 repo，core＋gateway＋cli-runtime）
- 單一資料表 `duduclaw-core/src/runtime_catalog.rs`：每個 runtime 的 id、顯示名、binary 名、安裝管道（npm 套件／安裝腳本／二進位）、headless 呼叫模板、輸出格式（text／json／jsonl）、model 旗標、登入方式（api_key env／device-code／browser-oauth）、憑證路徑、MCP 支援、模型家族前綴、ToS 註記。偵測、安裝、模型探索、`cli_auth::spec_for`、`infer_provider_for_model`、`CliKind` 全部改讀此表；`RuntimeType` 與 `CliKind` 統一。
- `which_cli` 候選加入 `/usr/bin`、`/usr/local/bin`、`/opt/duduclaw/runtimes/bin`。
- 新 runtime（`AgentRuntime` 實作，優先以通用 print-mode 模組 `runtime/generic_cli.rs` 承載，特殊協定才另立檔案）：Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro CLI、Cursor `agent`、Mistral Vibe、OpenCode。每個都要：headless 執行 → 文字結果；`--model` 對應；憑證存在偵測；登入流程規格（`CliAuthSpec`）；安裝規格。
- OTel：外層 `invoke_agent` span 的 `gen_ai.system/provider.name` 改依實際 runtime。
- DoD：`cargo test -p duduclaw-gateway runtime` 綠；每個新 runtime 至少一個以假 binary（shell script）驅動的整合測試；`docs/features/13-multi-runtime.md` 列出全部 runtime 與登入方式。

### WP-C OOBE 與殼（主 repo `crates/duduclaw-shell`）
- 「AI Runtime 授權」步驟改為 provider 清單：每家一列，兩個動作——「輸入 API 金鑰」（沿用現有欄位，`accounts.add` 帶 provider）與「登入帳號」（呼叫 gateway 的 CLI 登入 RPC，畫面顯示 device code／URL，或以映像內 Chromium 開啟登入頁；訂閱登入前顯示 §1-1 風險告知與勾選）。可多選；可略過。
- 完成頁摘要顯示已授權的 provider 數。
- DoD：`cargo test`（shell）綠；QEMU 活體：API key 路徑與至少一家 device-code 路徑走通。

### WP-D 本地模型（主 repo gateway＋dashboard）
- appliance 上偵測到 `llama-server`（`/usr/bin/llama-server`）時，`inference.toml` 預設 `backend = "openai_compat"`、`endpoint = http://127.0.0.1:8080/v1`、`models_dir = /data/duduclaw/models`。
- RPC：`inference.local.models`（精選 GGUF 清單：Qwen3 1.7B／4B／8B、Gemma 3 4B、Llama 3.2 3B 等 Q4_K_M，含大小與硬體建議）、`inference.local.download`（背景下載＋進度）、`inference.local.serve {model}`（寫 `/data/duduclaw/llama-server.env` 後 `systemctl restart duduclaw-llama-server`）、`inference.local.status`。
- `LocalModelsPage.tsx` 接上述 RPC；OOBE 可選「離線也能用：下載本地模型」。
- DoD：gateway 測試綠；dashboard build 綠；QEMU 活體：下載 1.7B 模型 → 啟動 → 交辦一件事由本地模型完成。

### WP-E 微調與後訓練（主 repo gateway＋dashboard）
- dashboard「微調與後訓練」頁：資料集建構（來源：agent 對話、審批決定、人工標註；輸出 ShareGPT／Alpaca SFT 與 DPO 偏好對）、訓練工作（後端：Together／Fireworks／OpenAI 微調 API 擇一先做，以及「自有 GPU 主機」——SSH 到指定主機執行 LLaMA-Factory CLI）、產物匯入（GGUF／LoRA → `/data/duduclaw/models` → 出現在本地模型頁）。
- gateway RPC：`finetune.datasets.*`、`finetune.jobs.*`、`finetune.import`。
- 資料集離開本機前顯示告知（內含客戶資料時）。
- DoD：gateway 測試綠；dashboard build 綠；以「自有 GPU 主機」後端做一次真跑（若無 GPU 主機則以 dry-run 驗證流程並如實標註）。

### WP-F OS 映像（DuDuClaw-OS repo）
- `nodejs`＋`nodejs-npm` 進桌面版映像。
- 新 recipe `duduclaw-ai-runtimes`：宿主端 `gen-ai-runtimes-bundle.sh`（在 linux/amd64 容器內 `npm install --prefix` 六個 npm 套件、下載 Grok／Kiro／Cursor／OpenCode 二進位、以 venv 打包 mistral-vibe）→ `file://duduclaw-ai-runtimes-<date>.tar.zst`，安裝到 `/opt/duduclaw/runtimes/{node_modules,bin,venv}`，`/usr/bin/<cli>` wrapper；沿用 `INHIBIT_PACKAGE_STRIP`／`INSANE_SKIP` 集合；`COMPATIBLE_MACHINE` 錨定。
- root 槽 `DUDUCLAW_AB_SLOT_SIZE_MB = 8192`；主 repo `os_update.rs::MAX_ROOT_BYTES` = 9 GiB（WP-B 一併改）。
- gateway drop-in 加 `Environment=HOME=/data/duduclaw`；`/data/duduclaw` 下各家 `.claude/.codex/.gemini/.qwen/.kimi/.copilot/.kiro/.vibe/.local/share/opencode` 由 gateway 建立。
- llama.cpp recipe（`llama-server`，Vulkan，x86-64-v3）＋`duduclaw-llama-server.service`（EnvironmentFile=/data/duduclaw/llama-server.env，模型存在才啟動）。
- `compat.d/llamafactory.toml` runner（docker：`hiyouga/llamafactory` 映像、LlamaBoard 7860 埠、明示需獨顯與 NVIDIA container toolkit）。
- DoD：`kas build` 綠；QEMU：各 CLI `--version` 可執行、`llama-server` 起得來；README／meta-duduclaw README／CHANGELOG 同步。

## 4. 整合與驗證順序

1. WP-A、WP-B、WP-D、WP-E 各在自己的 worktree 分支完成 → 由整合者合併到 main 工作樹（不 commit，交維護者）。
2. WP-C 依賴 WP-A（provider）與 WP-B（登入 RPC）合併後才能活體驗證。
3. WP-F 產出映像後，在 QEMU 跑：OOBE 授權（API key＋device code）、交辦（雲端）、本地模型下載與交辦（本地）、微調頁 dry-run、LlamaBoard runner 偵測。
4. 走查紀錄放 DuDuClaw-OS `wiki/eval/`，產品文件放 `docs/features/`（三語）。

## 5. 不做／延後

- Goose、Crush、Aider：多供應商殼層，與 OpenCode 重疊，延後。
- DeepSeek Harness／Deep Code、Trae、ERNIE：API key 型或無 Linux 穩定登入，走 `openai_compat` 預設集即可，不另做 runtime。
- 本機（CPU／內顯）訓練：不承諾；只提供資料集匯出與遠端訓練。

## 6. 第一輪結果（2026-09-06）

走查證據：DuDuClaw-OS `wiki/eval/ai-runtimes-qemu-walkthrough-2026-09-06.md`（QEMU TCG，映像 fix9 → fix11）。

| WP | 狀態 | 備註 |
|---|---|---|
| A 帳號 provider | COMPLETE | OOBE 存入兩家金鑰後 `config.toml` 有 `provider = "anthropic"/"openai"`、`accounts.list` 回 provider；dashboard 風險告知元件經 vitest。 |
| B runtime 註冊表＋7 款 runtime | COMPLETE（活體：偵測＋Claude 登入／交辦） | `runtime.detect` 12 列、10 款內建皆 installed；`RuntimeType::parse` 未知值拒絕；新 runtime 的 headless 交辦只以假 binary 整合測試，未在映像上實跑（需各家憑證）。 |
| C OOBE | COMPLETE | provider 清單可捲動、風險告知＋勾選、`claude setup-token` 真 URL、金鑰格式檢查、完成頁「已授權 2 家」。已知瑕疵：金鑰欄位 placeholder 對每家都顯示 `sk-ant-...`。 |
| D 本地模型 | PARTIAL | catalog／下載（1.1 GB）／`serve`／`status` 全部活體通過；交辦「由本地模型完成」在 TCG 下超過 300 s 逾時未觀察到完成，需真機。修了 `openai_compat` 線上名稱 bug。工具迴圈提示約 33k token 超過預設 8192 ctx → 退回 bare completion（待改）。 |
| E 微調 | PARTIAL（dry-run） | 資料集建構、`dry_run` 工作、artifacts 活體通過；`remote_gpu_ssh`／`together` 後端無 GPU 主機與金鑰，未真跑。 |
| F OS 映像 | COMPLETE（fix11 待最後複驗） | 十套 CLI `--version` 全過（補 `/lib64` loader）、llama-server 0.4.0、slot 8192、HOME drop-in、RemoteApp 登錄搬到 `/data/system/windows-vm`、LlamaBoard runner 宣告改 `linux-container`。Kiro 刻意不內建亦無殘根。Vulkan 未開（CPU x86-64-v3 建置）。 |

跨 WP 待辦（平台）——**第二輪（2026-09-06 晚）已全部修復並在 fix12 映像複驗**：goal loop 派工失敗即釋放名額＋退避＋連續 3 次轉人工、啟動清殘留 lease；`system.update_config` 的 `log_level` 改寫 `[general]`；本地工具迴圈依 llama.cpp `/props` 的 `n_ctx` 截短；OS 烤前主機磁碟防呆；OOBE 完成頁有線顯示。證據在同一份 wiki 的 §7；第三輪（fix13）另修「本地引擎首次探測失敗即整個程序停用」，改 60 秒重探＋`inference.local.serve/stop` 重設快取，證據 §8。
