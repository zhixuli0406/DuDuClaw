# TODO: ASR + TTS 語音功能整合

> 開始日期：2026-04-02 | 專案版本：v0.12.0+
> 目標：將現有孤島程式碼串接 + 加入免費 TTS 方案，實現零成本語音管線

---

## 現狀

| 元件 | 檔案 | 狀態 |
|------|------|------|
| Whisper API (ASR) | `crates/duduclaw-inference/src/whisper.rs` | ✅ 實作完成，❌ 未 export |
| Whisper Local (ASR) | 同上 (feature-gated) | ⚠️ `whisper-rs` 未加到 Cargo.toml |
| MiniMax T2A (TTS) | `crates/duduclaw-gateway/src/tts.rs` | ✅ 實作完成，❌ 未 export |
| Media Pipeline | `crates/duduclaw-gateway/src/media.rs` | ✅ MIME 偵測，❌ 缺音訊解碼 |
| `/voice` 指令 | `crates/duduclaw-gateway/src/chat_commands.rs` | ⚠️ 存在但未接線到 TTS |
| Telegram 語音 | `crates/duduclaw-gateway/src/telegram.rs` | ⚠️ 收到但未轉錄 |

---

## Phase 1：補完串接（目標：讓現有程式碼能用）

### 1.1 模組 Export [P0] ✅

- [x] `crates/duduclaw-gateway/src/lib.rs` — 加入 `pub mod tts;` + `pub mod media;`
- [x] `crates/duduclaw-inference/src/lib.rs` — 加入 `pub mod whisper;` + `pub mod asr;` + `pub mod audio_decode;`
- [x] 修復依賴：gateway 加入 `async-trait` + `image`；inference 加入 `multipart` feature

### 1.2 音訊解碼 — symphonia [P0] ✅

- [x] `crates/duduclaw-inference/Cargo.toml` — 加入 `symphonia` 依賴（ogg/mp3/isomp4/aac/wav/flac/pcm）
- [x] `crates/duduclaw-inference/src/audio_decode.rs` — 新增音訊解碼模組（168 行）
  - [x] `decode_to_pcm(data: &[u8]) -> Result<Vec<f32>>` — 任意格式 → PCM f32 16kHz mono
  - [x] 支援 OGG Opus（Telegram voice）、M4A/AAC（LINE audio）、MP3、WAV、FLAC
  - [x] Magic bytes 自動偵測格式
  - [x] 線性插值重採樣（任意 sample rate → 16kHz）
  - [x] 自動 stereo → mono 混音
  - [x] 3 個單元測試

### 1.3 AsrProvider trait [P0] ✅

- [x] `crates/duduclaw-inference/src/asr.rs` — ASR provider trait + 實作（141 行）
  - [x] `AsrProvider` trait（`transcribe(&self, pcm: &[f32], lang: &str) -> Result<String>`）
  - [x] `WhisperApiProvider` — 透過 OpenAI Whisper API，自動 PCM → WAV 編碼
  - [x] `pcm_to_wav()` — 最小 WAV 檔案產生器（44 byte header + PCM i16）
  - [x] 2 個單元測試（WAV header 驗證 + provider name）
- [x] `WhisperLocalProvider` — whisper.cpp 本地 ASR（feature-gated `whisper`）
  - [x] `whisper-rs = "0.13"` 加入 Cargo.toml（optional）
  - [x] `WhisperLocalProvider::new()` — 路徑驗證 + 檔案存在檢查
  - [x] `from_models_dir()` — 自動掃描 `~/.duduclaw/models/whisper/ggml-*.bin`
  - [x] `spawn_blocking` 包裝 CPU 密集推理
  - [x] asr_router `auto_detect` 已加入 WhisperLocal

### 1.4 edge-tts Provider [P1] ✅

- [x] `crates/duduclaw-gateway/src/tts.rs` — 新增 `EdgeTtsProvider`（~120 行）
  - [x] WebSocket 連線到 `wss://speech.platform.bing.com/...`
  - [x] zh-TW 預設聲音：`zh-TW-HsiaoChenNeural`（女）
  - [x] en-US 預設聲音：`en-US-AriaNeural`（女）
  - [x] 可自訂聲音（`with_voices()`）
  - [x] 輸出 MP3 bytes（audio-24khz-48kbitrate-mono-mp3）
  - [x] SSML 建構 + XML escape
  - [x] 自動 CJK 語言偵測
  - [x] Binary frame 解析（header tag 定位 + audio chunk 收集）
  - [x] `turn.end` 訊號完成偵測
- [ ] `TtsRouter` — 依設定選擇 provider（edge-tts → MiniMax → fallback 文字）— Phase 3

### 1.5 語音收發串接 [P1] ✅

- [x] Telegram 語音接收
  - [x] `telegram.rs` — `TgMessage` 加入 `voice: Option<TgVoice>` + `audio: Option<TgAudio>` 欄位
  - [x] `transcribe_voice()` — getFile → 下載 OGG → Whisper API 轉錄
  - [x] 語音訊息自動轉為文字進入 Agent pipeline
  - [x] 錯誤處理：轉錄失敗時回傳 ⚠️ 提示
- [x] TTS 語音回覆
  - [x] `channel_reply.rs` — `ReplyContext` 加入 `voice_sessions: HashSet<String>`
  - [x] `chat_commands.rs` — `/voice` 指令切換 session voice mode（toggle on/off）
  - [x] `telegram.rs` — voice mode 時呼叫 `EdgeTtsProvider::synthesize()` → `send_voice()`
  - [x] `send_voice()` — Telegram `/sendVoice` multipart API
  - [x] 長文回覆時同時發送文字摘要（前 200 字）
  - [x] TTS 失敗時自動 fallback 到文字回覆
- [x] 設定（`VoiceConfig` in `inference.toml [voice]`）
  - [x] `asr_provider` — "auto" / "whisper-api" / "whisper-local" / "sensevoice"
  - [x] `tts_provider` — "auto" / "edge-tts" / "minimax" / "openai-tts" / "piper"
  - [x] `asr_language` — BCP-47 語言提示（預設 "zh"）
  - [x] `tts_voice` — 聲音名稱（空 = 自動偵測）
  - [x] `voice_reply_enabled` — 預設語音回覆模式

### 1.6 MCP 工具 [P2] ✅

- [x] `transcribe_audio` — 接收 base64 音訊 → Whisper API → 回傳文字
  - [x] 支援 OGG/MP3/WAV/M4A（格式自動偵測）
  - [x] `language` 參數（預設 zh）
- [x] `synthesize_speech` — 接收文字 → edge-tts → 回傳 base64 MP3
  - [x] `voice` 參數（預設自動偵測 zh-TW/en-US）
  - [x] `tool_text()` / `tool_error()` 統一回傳格式

---

## Phase 2：本地 ASR 強化 ✅

- [x] SenseVoice ONNX 整合（`sensevoice.rs`，`ort` crate feature-gated）
  - [x] `SenseVoiceProvider` — `AsrProvider` trait 實作
  - [x] ONNX Session 載入 + intra_threads(4)
  - [x] 語言 ID 映射（zh=0, en=1, ja=2, ko=3）
  - [x] feature-gated: 需啟用 `onnx` feature
- [x] Silero VAD 預處理（`vad.rs`）
  - [x] `SileroVad` — ONNX 模型載入 + 語音段偵測
  - [x] `VadConfig` — 可調閾值、最小語音/靜音時長、視窗大小
  - [x] `detect_speech()` → `Vec<SpeechSegment>` (start, end, confidence)
  - [x] `extract_speech()` — 從 PCM 提取語音段
  - [x] 3 個單元測試
- [x] ASR Router（`asr_router.rs`）
  - [x] `AsrRouter` — 多 provider fallback（SenseVoice → Whisper Local → Whisper API）
  - [x] `AsrStrategy`：LocalFirst / CloudOnly / LocalOnly
  - [x] VAD 預處理整合（有 VAD 時自動過濾靜音）
  - [x] `auto_detect()` — 從 models_dir 自動掃描可用 provider
  - [x] 1 個單元測試
- [x] `whisper-rs` Cargo.toml 依賴已修復（feature flag `whisper` + `whisper-rs = "0.13"`）

## Phase 3：TTS 多 Provider ✅

- [x] OpenAI TTS provider（`tts.rs` `OpenAiTtsProvider`）
  - [x] `tts-1` / `tts-1-hd` 模型支援
  - [x] 自動語言偵測（CJK → nova, 英文 → alloy）
  - [x] 從 `OPENAI_API_KEY` 環境變數載入
- [x] Piper TTS 本地方案（`tts.rs` `PiperTtsProvider`）
  - [x] subprocess 呼叫 `piper` CLI
  - [x] `from_models_dir()` 自動掃描 `~/.duduclaw/models/piper/*.onnx`
  - [x] PCM i16 → WAV 編碼器
- [x] TTS Router（`tts.rs` `TtsRouter`）
  - [x] `TtsStrategy`：LocalFirst / EdgeOnly / CloudBest
  - [x] `auto_detect()` — Piper → edge-tts → MiniMax → OpenAI fallback
  - [x] 逐 provider 嘗試，失敗自動降級
- [x] 語音設定 i18n（`en.json` + `zh-TW.json`）
  - [x] `voice.*` 系列 14 個鍵值（標題、Provider 名稱、語言、開關）

## Phase 4：即時語音架構 ✅

- [x] `realtime_voice.rs` — 即時語音對話架構設計（trait + types）
  - [x] `StreamingAsrProvider` — 串流 ASR trait（mpsc audio → mpsc transcript）
  - [x] `StreamingTtsProvider` — 串流 TTS trait（text → mpsc audio chunks）
  - [x] `PartialTranscript` — 部分辨識結果（含 is_final, confidence, language）
  - [x] `AudioChunk` — 音訊片段（含 format: PCM/MP3/OGG）
  - [x] `VoiceSessionState` — 狀態機（Listening → Recognizing → Thinking → Speaking）
  - [x] `VoiceEvent` — 事件系統（SpeechStart/Transcript/BargeIn/AudioReady...）
  - [x] `VoiceSessionConfig` — 設定（語言、聲音、打斷、靜音逾時）
  - [x] 2 個單元測試
- [x] LiveKit WebRTC 整合 — `livekit_voice.rs`
  - [x] `LiveKitConfig` — 伺服器 URL、API key/secret、房間名、bot identity
  - [x] `LiveKitSession` trait — connect/events/publish_audio/disconnect
  - [x] `LiveKitStub` — 無 feature flag 時的 stub 實作
  - [x] `create_session()` factory — 根據 feature flag 回傳真實或 stub
  - [x] 2 個單元測試（stub 行為驗證）
  - [x] 真實 `livekit` crate 整合（feature `livekit-voice`）
    - [x] `livekit = "0.7"` + `livekit-api = "0.4"` optional 依賴
    - [x] `RealLiveKitSession` — Room 連線 + token 生成 + audio track 訂閱
    - [x] `NativeAudioStream` → PCM → `VoiceEvent::AudioReady` 串流
    - [x] participant disconnect + room disconnect 事件處理
    - [x] `create_session()` 依 feature flag 回傳 Real 或 Stub
- [x] 串流 ASR 實作 — `deepgram.rs` `DeepgramStreamingAsr`
  - [x] `StreamingAsrProvider` trait 實作（mpsc audio → mpsc transcript）
  - [x] WebSocket 連線到 `wss://api.deepgram.com/v1/listen`
  - [x] PCM f32 → i16 即時轉換 + 串流傳送
  - [x] 解析 Deepgram JSON 回應（partial + final transcript + confidence + language）
  - [x] 10s 連線 timeout + API key zeroize
  - [x] 可設定 model（nova-3）+ language
  - [x] 2 個單元測試
- [x] Discord Voice Channel 支援 — `discord_voice.rs`
  - [x] `DiscordVoiceConfig` — 啟用開關、ASR 語言、TTS 聲音、靜音逾時、最大頻道數
  - [x] `DiscordVoiceManager` — 多頻道 session 管理（join/leave/state 追蹤）
  - [x] `UserAudioBuffer` — 每用戶音訊累積（48kHz stereo i16 → 16kHz mono f32 轉換）
  - [x] `process_voice_tick()` — 靜音偵測 → ASR-ready PCM 批次產出
  - [x] 3:1 降頻（48kHz→16kHz）+ stereo→mono 混音
  - [x] 自動清理 30s 未說話的用戶 buffer
  - [x] 5 個單元測試（轉換、duration、join 限制、tick 處理）
  - [x] Songbird crate 實際接入（feature `discord-voice`）
    - [x] `songbird = "0.5"` optional 依賴（driver + rustls + receive）
    - [x] `SongbirdReceiver` — VoiceTick 事件處理 → per-user PCM 累積 → ASR 管線
    - [x] `SpeakingStateHandler` — 說話狀態日誌
    - [x] `join_voice_channel()` — Songbird join + DecodeMode + 事件註冊 → mpsc ASR 輸出
    - [x] `leave_voice_channel()` — 離開 + 狀態清理
    - [x] SSRC → user audio 映射 + 48kHz stereo Opus decode
