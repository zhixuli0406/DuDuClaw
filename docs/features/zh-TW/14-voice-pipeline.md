# 語音管線

> HTTP 與 Telegram 的語音辨識、語音合成，共用供應商設定。

---

## 歷史說明

2026-09 之前，本頁描述的是一套四元件的本地優先管線：SenseVoice ONNX ＋ Deepgram ASR、Silero VAD、`symphonia` 音訊解碼、LiveKit 多 agent 語音房。

對整個 repo grep `sensevoice`、`deepgram`、`silero`、`symphonia`、`livekit`，**沒有任何程式碼、也沒有任何依賴**——只有 `.cargo/audit.toml` 裡一行註解把 `livekit-api` 寫成某條 RUSTSEC 豁免的理由，而 `Cargo.lock` 裡根本沒有那個 crate。這四項從未出貨過。

以下是真正存在的東西。

---

## 共用語音設定

HTTP 與 Telegram 使用 `config.toml [voice] stt_*` 設定語音辨識。HTTP、Telegram 語音回覆與 `synthesize_speech` MCP 工具共用 `TtsRouter::for_home`，依 `inference.toml [voice] tts_provider`／`tts_voice` 設定合成語音。

### 路徑 1 — HTTP 端點（儀表板、WebChat）

| 端點 | 驗證 | 行為 |
|---|---|---|
| `POST /api/stt` | Bearer JWT | body 上限 10 MiB。**在碰 body 之前**先從 `config.toml [voice]` 解析供應商，沒設定就回 **501 fail-closed**——絕不猜測、絕不編造逐字稿 |
| `POST /api/tts` | Bearer JWT | 使用共用 router：`edge-tts`、`piper`、`minimax`、`openai-tts` 各只使用指定供應商；其他啟用設定採本機優先。設成 `none`／`off`／`disabled` 時回 501 |
| `GET`／`POST /api/voice/config` | Bearer JWT | 讀寫 `[voice]` 的 STT 設定 |

**STT 供應商**（`stt.rs`，兩個實作）：

- `OpenAiCompatStt` — `POST {base_url}/audio/transcriptions`，multipart 帶 bearer key。OpenAI Whisper 與 Groq Whisper 都講這個形狀。
- `CommandStt` — 本機子行程樣板（例如 `whisper-cli`）；音訊寫到暫存檔、餵給指令、從 stdout 讀回逐字稿。

```toml
[voice]
stt_provider  = "openai_compat"   # 或 "command"；未設定 ⇒ /api/stt 回 501
stt_base_url  = "https://api.openai.com/v1"
stt_api_key   = "sk-..."          # 或 stt_api_key_enc（AES-256-GCM）
stt_model     = "whisper-1"
stt_command   = "whisper-cli -m /models/ggml-base.bin -f {audio} --output-txt --no-prints"
```

**TTS 供應商**（`tts.rs`，同一個 `TtsProvider` trait 下四個實作）：

| 供應商 | 位置 | 說明 |
|---|---|---|
| `PiperTtsProvider` | 本機 | 從模型目錄讀 ONNX 語音 |
| `EdgeTtsProvider` | 雲端 | 選用、須另行安裝的 `edge-tts` CLI；不用 API key |
| `MiniMaxTts` | 雲端 | T2A v2；分析文字內容自動挑 CJK 或拉丁語音 |
| `OpenAiTtsProvider` | 雲端 | 依字元計費 |

`TtsRouter` 支援本機優先（Piper → edge-tts → MiniMax → OpenAI）、雲端優先與指定單一供應商，只使用偵測到且產品政策允許的供應商。

Edge TTS 為選用功能，必須另行執行 `pipx install edge-tts` 安裝，並以 `pipx upgrade edge-tts` 更新。它使用 Microsoft 非官方端點，可能隨時失效。DuDuClaw 以獨立子行程呼叫 CLI，不內含該 Python 套件。`duduclaw doctor` 會列出本機可用供應商，不會連線測試。

### 路徑 2 — Telegram 語音訊息

語音或音訊訊息會觸發 `transcribe_voice`：bot 呼叫 `getFile`，對回傳的 `file_path` 做路徑穿越驗證（`..`、絕對路徑、NUL、非白名單字元），下載時在 `content_length` 與實際位元組兩處都檢查大小上限，然後轉錄。

**v1.68.0 起它呼叫 `stt::transcribe_channel_audio`，使用儀表板的語音轉文字設定（`config.toml [voice] stt_*`，與 `/api/stt` 同一個供應商）。** 只有沒設定語音轉文字供應商時，才退回用環境變數 `OPENAI_API_KEY` 的 OpenAI Whisper；已設定的供應商失敗時直接回報錯誤，不退回。語言仍固定為 `zh`。LINE 的語音訊息走同一條路徑。v1.68.0 以前這兩個通道一律走環境變數金鑰的 Whisper。

Telegram 語音回覆（每個聊天室用 `/voice` 切換）透過共用 TTS router 合成純文字。沒有可用供應商時隱藏切換按鈕，`/voice` 保持關閉。合成失敗時退回文字，每個 session 第一次失敗另附簡短通知；音訊上傳失敗也保留原有文字回覆。

v1.67.1 起，「語音」分頁拿掉了「語音回覆模式」「語音辨識」「語言」三個欄位。它們寫入的 `voice_reply_enabled`、`asr_provider`、`asr_language` 在 gateway 裡沒有任何程式讀取。已經存在 `inference.toml` 的值保持原樣，分頁儲存時不再送這三個鍵；v1.68.0 起設定結構也拿掉了這三個欄位，`system.update_config` 收到時忽略。`/api/stt` 的語音轉文字設定在同一分頁的進階卡片（`config.toml [voice] stt_*`）。

---

## 本機 Whisper

`duduclaw-inference::whisper` 有兩種模式：`Api`（OpenAI Whisper API）與 `Local { model_path }`（透過 `whisper-rs` 的 whisper.cpp）。本機模式在**非預設的 `whisper` Cargo feature** 之後——該 crate 的 `default` 是 `["cpu"]`——所以出廠 release binary 只有 API 模式。

---

## Discord 語音

`discord_voice.rs` 在**非預設的 `discord-voice` Cargo feature** 之後包裝 **Songbird**；gateway 的 `default` 是 `["dashboard", "desktop"]`，所以出廠 build 不含它。它背後今天也沒有 VAD、沒有接 ASR——這個模組涵蓋的是加入頻道與播放音訊。

---

## ONNX embedding

`duduclaw-inference` 的 `onnx` feature 啟用 `OnnxEmbeddingProvider`（ONNX Runtime ＋ WordPiece tokenizer），用於 `bge-small-zh` 之類的模型。它不是語音元件，而是記憶系統語意搜尋共用的基礎設施（見 [10-cognitive-memory.md](10-cognitive-memory.md)），而且同樣**不在**該 crate 的 default features 裡。

---

## 與其他系統的互動

- **通道** — Telegram 會自動轉錄語音與音訊訊息，`/voice` 逐聊天室切換語音回覆。其他通道沒有語音路徑。
- **Multi-runtime** — 逐字稿就是普通文字：agent 用哪個 runtime 就由誰處理。
- **記憶** — 語音輪次以普通情節記憶儲存（逐字稿＋metadata）。
- **去識別化** — 從 `/api/stt` 進來的逐字稿與其他文字一樣，套用同一套 MCP 側去識別化規則；Telegram 路徑把逐字稿餵進一般回覆路徑。

---

## 總結

HTTP、Telegram 語音回覆與 MCP 合成工具共用 TTS 設定。偵測到供應商不代表端點連線已通過驗證；Telegram 合成失敗時會退回文字。
