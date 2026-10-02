# 語音管線

> 語音進、語音出——兩條目前不共用設定的路徑，以及本頁原本宣稱、但程式碼裡不存在的四個元件。

---

## 歷史說明

2026-09 之前，本頁描述的是一套四元件的本地優先管線：SenseVoice ONNX ＋ Deepgram ASR、Silero VAD、`symphonia` 音訊解碼、LiveKit 多 agent 語音房。

對整個 repo grep `sensevoice`、`deepgram`、`silero`、`symphonia`、`livekit`，**沒有任何程式碼、也沒有任何依賴**——只有 `.cargo/audit.toml` 裡一行註解把 `livekit-api` 寫成某條 RUSTSEC 豁免的理由，而 `Cargo.lock` 裡根本沒有那個 crate。這四項從未出貨過。

以下是真正存在的東西。

---

## 兩條路徑、兩套設定

最重要的一點：**HTTP 語音端點與 Telegram 語音處理是分開接線的，而 `[voice]` 設定只到得了其中一條。**

### 路徑 1 — HTTP 端點（儀表板、WebChat）

| 端點 | 驗證 | 行為 |
|---|---|---|
| `POST /api/stt` | Bearer JWT | body 上限 10 MiB。**在碰 body 之前**先從 `config.toml [voice]` 解析供應商，沒設定就回 **501 fail-closed**——絕不猜測、絕不編造逐字稿 |
| `POST /api/tts` | Bearer JWT | 每次請求讀 `inference.toml [voice] tts_provider`／`tts_voice`，據此選 `TtsRouter` 策略（`edge-tts` → 只用 edge，`minimax`／`openai-tts` → 雲端優先，其他 → 本地優先）；設成 `none`／`off`／`disabled` 時回 501 |
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
| `EdgeTtsProvider` | 雲端 | 免費、不用 API key |
| `MiniMaxTts` | 雲端 | T2A v2；分析文字內容自動挑 CJK 或拉丁語音 |
| `OpenAiTtsProvider` | 雲端 | 依字元計費 |

`TtsRouter` 以三種策略之一在它們之間分派：`LocalFirst`（本機 → edge → MiniMax → OpenAI）、`EdgeOnly`、`CloudBest`。

### 路徑 2 — Telegram 語音訊息

語音或音訊訊息會觸發 `transcribe_voice`：bot 呼叫 `getFile`，對回傳的 `file_path` 做路徑穿越驗證（`..`、絕對路徑、NUL、非白名單字元），下載時在 `content_length` 與實際位元組兩處都檢查大小上限，然後轉錄。

**接著它呼叫 `duduclaw_inference::whisper::transcribe(bytes, Some("zh"), WhisperMode::Api)`——供應商與語言都是寫死的。** 語音回覆（每個聊天室用 `/voice` 切換）直接 new 一個 `EdgeTtsProvider`，同樣寫死；音訊上傳失敗時會退回純文字。

所以儀表板「語音」分頁寫入的 `inference.toml [voice] tts_provider`／`tts_voice` 只影響 `POST /api/tts`，**目前對 Telegram 路徑沒有任何影響**。在 UI 改了它們，Telegram 的語音訊息行為不會變。這是已知缺口，寫在這裡，免得操作者自己撞上。

v1.67.1 起，「語音」分頁拿掉了「語音回覆模式」「語音辨識」「語言」三個欄位。它們寫入的 `voice_reply_enabled`、`asr_provider`、`asr_language` 在 gateway 裡沒有任何程式讀取。已經存在 `inference.toml` 的值保持原樣，分頁儲存時不再送這三個鍵。`/api/stt` 的語音轉文字設定在同一分頁的進階卡片（`config.toml [voice] stt_*`）。

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

語音在兩個地方會動，而誠實版本就直說：一組由儀表板設定的 fail-closed HTTP 端點，加上一個供應商寫死的 Telegram handler。點名第二個，比畫一張從來沒寫過的 VAD 流程圖有用得多。
