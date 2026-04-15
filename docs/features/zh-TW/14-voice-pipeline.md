# 語音管線

> 本地優先的語音智慧——ASR、TTS、VAD 與即時語音房。

---

## 比喻：同步口譯員的工具箱

一位專業同步口譯員需要三樣東西：

1. **耳朵** — 聽懂說話者（ASR：語音轉文字）
2. **嗓音** — 清晰地說出翻譯（TTS：文字轉語音）
3. **判斷力** — 知道說話者何時在說話、何時停頓（VAD：語音活動偵測）

而在會議口譯中，還需要一個**口譯間**——一個可控的環境，讓多位口譯員同時處理不同語言對（LiveKit 語音房）。

DuDuClaw 的語音管線提供全部四個組件，且強烈偏好本地處理——你的語音資料留在你的機器上，除非你明確將它路由到雲端。

---

## 運作方式

### ASR（自動語音辨識）

四種供應商，按隱私和成本排序：

| 供應商 | 位置 | 速度 | 語言 | 成本 |
|--------|------|------|------|------|
| **SenseVoice (ONNX)** | 本地 | 快 | CJK + 多語 | 免費 |
| **Whisper.cpp** | 本地 | 中等 | 99 種語言 | 免費 |
| **OpenAI Whisper API** | 雲端 | 快 | 57 種語言 | 按分鐘計費 |
| **Deepgram** | 雲端 | 非常快 | 36 種語言 | 按分鐘計費 |

系統依據 `inference.toml` 的設定選擇供應商：

```toml
[inference.voice]
asr_provider = "auto"    # auto / sensevoice / whisper-local / whisper-api / deepgram
asr_language = "zh"      # 語言偵測提示
```

設為 `auto` 時，系統優先使用本地供應商：

```
收到語音訊息（OGG Opus / MP3 / WAV）
     |
     v
音訊解碼（symphonia：OGG/MP3/AAC/WAV/FLAC → PCM）
     |
     v
SenseVoice ONNX 可用？
  +--+--+
  |     |
 是     否 → Whisper.cpp 可用？
  |          +--+--+
  |          |     |
  |         是     否 → 降級到雲端 API
  |          |
  v          v
本地 ASR → 轉錄文字
```

**Telegram 整合**：當使用者發送語音訊息，Bot 自動下載 OGG Opus 檔案、解碼為 PCM、轉錄，並將文字當作使用者打字的訊息處理。回應可選擇以語音訊息形式發送回去（透過 TTS）。

### TTS（文字轉語音）

四種供應商，各有不同優勢：

| 供應商 | 位置 | 品質 | 語言 | 成本 |
|--------|------|------|------|------|
| **Piper (ONNX)** | 本地 | 良好 | 30+ 種語言 | 免費 |
| **MiniMax T2A** | 雲端 | 優秀 | CJK + 拉丁語系（自動偵測）| 按字元計費 |
| **Edge TTS** | 雲端 | 良好 | 400+ 種聲音 | 免費 |
| **OpenAI TTS** | 雲端 | 優秀 | 多語 | 按字元計費 |

MiniMax T2A 供應商包含**自動語言偵測**：它分析文字內容判斷是 CJK（中文/日文/韓文）還是拉丁語系，並據此選擇適當的語音模型。

```toml
[inference.voice]
tts_provider = "auto"    # auto / piper / minimax / edge-tts / openai-tts
tts_voice = ""           # 空值 = 從文字語言自動偵測
voice_reply_enabled = false  # 設為 true 啟用語音回應
```

### VAD（語音活動偵測）

**Silero VAD**（ONNX）在本地運行，偵測使用者何時在說話、何時沉默。這對以下場景至關重要：

- **Discord 語音頻道**：知道何時開始/停止錄音
- **LiveKit 語音房**：管理多 Agent 對話中的發言順序
- **串流 ASR**：將連續音訊分割成獨立語句

```
連續音訊串流
     |
     v
Silero VAD（本地 ONNX 模型）
     |
     v
偵測到語音？
  +--+--+
  |     |
 是     否
  |     |
  v     v
開始    停止
錄音    錄音
  |
  v
將片段送往 ASR
```

### 音訊解碼

在任何語音處理之前，音訊檔案需要解碼為原始 PCM。`symphonia` crate 支援：

- **OGG Opus** — Telegram 語音訊息
- **MP3** — 常見音訊格式
- **AAC** — iPhone 語音備忘錄
- **WAV** — 未壓縮音訊
- **FLAC** — 無損壓縮音訊

所有格式統一解碼為 16kHz 單聲道 PCM 串流以供 ASR 處理。

---

## 即時語音：Discord 與 LiveKit

### Discord 語音頻道

DuDuClaw 透過 **Songbird**（Rust Discord 語音函式庫）整合 Discord 語音頻道：

```
使用者加入 Discord 語音頻道
     |
     v
Bot 加入同一頻道（Songbird）
     |
     v
VAD 偵測語音 → ASR 轉錄
     |
     v
Agent 處理轉錄文字
     |
     v
TTS 產生音訊回應
     |
     v
Bot 在語音頻道播放音訊
```

這讓 Discord 中的自然語音對話成為可能——Agent 聆聽、理解、然後回話。

### LiveKit 語音房

對於多 Agent 語音協作，DuDuClaw 支援 **LiveKit** WebRTC 語音房：

```
LiveKit 語音房
  ├── Agent A（客服）
  ├── Agent B（技術專家）
  ├── Agent C（翻譯）
  └── 人類使用者
     |
     v
每個 Agent 獨立：
  - 透過 VAD + ASR 聆聽
  - 透過各自的 runtime 處理
  - 透過 TTS 回應
  - 管理發言順序
```

LiveKit 房間支援以下場景：
- **轉接對話**：客服 Agent 升級到專家，專家加入同一房間
- **多語會議**：翻譯 Agent 即時彌合語言隔閡
- **協作除錯**：多個專業 Agent 共同討論問題

---

## ONNX 嵌入

語音管線與**嵌入引擎**共用基礎設施——兩者都使用 ONNX Runtime 進行本地模型執行。嵌入元件提供：

- **BERT WordPiece tokenizer** 文字分詞
- **ONNX Runtime** 向量嵌入產生
- 支援 `bge-small-zh` 和 `qwen3-embedding-0.6b` 等模型

這些嵌入驅動記憶系統的語意搜尋（參見[認知記憶系統](10-cognitive-memory.md)）。

---

## 為什麼這很重要

### 隱私

本地 ASR 和 TTS 意味著語音資料永遠不會離開機器。對於醫療、法律或合規敏感的場景，這是不可妥協的。

### 成本

雲端 ASR/TTS 服務按分鐘/字元收費。本地模型（SenseVoice、Piper）在初次下載後免費。對於高流量語音互動，節省非常可觀。

### 延遲

本地模型消除了網路往返。在現代 GPU 上，語音訊息可以在毫秒內完成轉錄，而雲端 API 呼叫需要數秒。

### 自然互動

語音將 Agent 從文字工具轉變為對話夥伴。使用者可以在開車、做飯或散步時互動——不需要打字。

---

## 與其他系統的互動

- **通道整合**：Telegram 語音訊息自動轉錄；Discord 語音頻道即時語音。
- **Multi-Runtime**：轉錄後的語音由 Agent 設定的 runtime 處理。
- **記憶系統**：語音互動以情節記憶形式儲存（轉錄 + 元資料）。
- **Confidence Router**：語音查詢和其他查詢一樣路由——簡單問候走本地，複雜問題走雲端。
- **推論引擎**：共用 ONNX Runtime 基礎設施，高效執行本地模型。

---

## 總結

語音是最自然的人機介面。DuDuClaw 的語音管線讓它變得可及，同時不犧牲隱私也不超出預算——本地優先的 ASR 和 TTS 免費處理常見情況，雲端供應商處理邊緣案例，而 LiveKit 則支撐多 Agent 語音協作的未來。
