# Voice Pipeline

> Local-first speech intelligence — ASR, TTS, VAD, and real-time voice rooms.

---

## The Metaphor: A Simultaneous Interpreter's Toolkit

A professional simultaneous interpreter needs three things:

1. **Ears** — to hear and understand the speaker (ASR: speech-to-text)
2. **Voice** — to speak the translation clearly (TTS: text-to-speech)
3. **Judgment** — to know when the speaker is talking vs. pausing (VAD: voice activity detection)

And for conference interpreting, they need a **booth** — a controlled environment where multiple interpreters can work on different language pairs simultaneously (LiveKit voice rooms).

DuDuClaw's voice pipeline provides all four components, with a strong preference for local processing — your voice data stays on your machine unless you explicitly route it to the cloud.

---

## How It Works

### ASR (Automatic Speech Recognition)

Four providers, ordered by privacy and cost:

| Provider | Location | Speed | Languages | Cost |
|----------|----------|-------|-----------|------|
| **SenseVoice (ONNX)** | Local | Fast | CJK + multilingual | Free |
| **Whisper.cpp** | Local | Moderate | 99 languages | Free |
| **OpenAI Whisper API** | Cloud | Fast | 57 languages | Pay-per-minute |
| **Deepgram** | Cloud | Very fast | 36 languages | Pay-per-minute |

The system selects the provider based on configuration in `inference.toml`:

```toml
[inference.voice]
asr_provider = "auto"    # auto / sensevoice / whisper-local / whisper-api / deepgram
asr_language = "zh"      # Hint for language detection
```

When set to `auto`, the system prefers local providers:

```
Voice message received (OGG Opus / MP3 / WAV)
     |
     v
Audio decode (symphonia: OGG/MP3/AAC/WAV/FLAC → PCM)
     |
     v
SenseVoice ONNX available?
  +--+--+
  |     |
 Yes    No → Whisper.cpp available?
  |          +--+--+
  |          |     |
  |         Yes    No → Fall to cloud API
  |          |
  v          v
Local ASR → transcription text
```

**Telegram integration**: When a user sends a voice message, the bot automatically downloads the OGG Opus file, decodes it to PCM, transcribes it, and processes the text as if the user had typed it. The response can optionally be sent back as a voice message (via TTS).

### TTS (Text-to-Speech)

Four providers, each with different strengths:

| Provider | Location | Quality | Languages | Cost |
|----------|----------|---------|-----------|------|
| **Piper (ONNX)** | Local | Good | 30+ languages | Free |
| **MiniMax T2A** | Cloud | Excellent | CJK + Latin (auto-detect) | Pay-per-character |
| **Edge TTS** | Cloud | Good | 400+ voices | Free |
| **OpenAI TTS** | Cloud | Excellent | Multilingual | Pay-per-character |

The MiniMax T2A provider includes **automatic language detection**: it analyzes the text content to determine if it's CJK (Chinese/Japanese/Korean) or Latin-script, and selects the appropriate voice model accordingly.

```toml
[inference.voice]
tts_provider = "auto"    # auto / piper / minimax / edge-tts / openai-tts
tts_voice = ""           # Empty = auto-detect from text language
voice_reply_enabled = false  # Set true to enable voice responses
```

### VAD (Voice Activity Detection)

**Silero VAD** (ONNX) runs locally to detect when a user is speaking vs. silent. This is critical for:

- **Discord voice channels**: Knowing when to start/stop recording
- **LiveKit voice rooms**: Managing turn-taking in multi-agent conversations
- **Streaming ASR**: Segmenting continuous audio into individual utterances

```
Continuous audio stream
     |
     v
Silero VAD (local ONNX model)
     |
     v
Speech detected? 
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Start  Stop
recording  recording
  |
  v
Send segment to ASR
```

### Audio Decoding

Before any speech processing, audio files need to be decoded to raw PCM. The `symphonia` crate handles this with support for:

- **OGG Opus** — Telegram voice messages
- **MP3** — Common audio format
- **AAC** — iPhone voice memos
- **WAV** — Uncompressed audio
- **FLAC** — Lossless compressed audio

All formats are decoded to a uniform 16kHz mono PCM stream for ASR processing.

---

## Real-Time Voice: Discord & LiveKit

### Discord Voice Channels

DuDuClaw integrates with Discord voice channels via **Songbird** (Rust Discord voice library):

```
User joins Discord voice channel
     |
     v
Bot joins the same channel (Songbird)
     |
     v
VAD detects speech → ASR transcribes
     |
     v
Agent processes transcription
     |
     v
TTS generates audio response
     |
     v
Bot plays audio in voice channel
```

This enables natural voice conversations in Discord — the agent listens, understands, and speaks back.

### LiveKit Voice Rooms

For multi-agent voice collaboration, DuDuClaw supports **LiveKit** WebRTC voice rooms:

```
LiveKit voice room
  ├── Agent A (customer support)
  ├── Agent B (technical specialist)
  ├── Agent C (translator)
  └── Human user
     |
     v
Each agent independently:
  - Listens via VAD + ASR
  - Processes through its own runtime
  - Responds via TTS
  - Manages turn-taking
```

LiveKit rooms enable scenarios like:
- **Handoff conversations**: Customer support agent escalates to a specialist, who joins the same room
- **Multilingual meetings**: A translator agent bridges language gaps in real-time
- **Collaborative debugging**: Multiple specialized agents discuss a problem together

---

## ONNX Embedding

The voice pipeline shares infrastructure with the **embedding engine** — both use ONNX Runtime for local model execution. The embedding component provides:

- **BERT WordPiece tokenizer** for text tokenization
- **ONNX Runtime** for vector embedding generation
- Support for models like `bge-small-zh` and `qwen3-embedding-0.6b`

These embeddings power the memory system's semantic search (see [Cognitive Memory](10-cognitive-memory.md)).

---

## Why This Matters

### Privacy

Local ASR and TTS mean voice data never leaves the machine. For healthcare, legal, or compliance-sensitive contexts, this is non-negotiable.

### Cost

Cloud ASR/TTS services charge per minute/character. Local models (SenseVoice, Piper) are free after the initial download. For high-volume voice interactions, the savings are substantial.

### Latency

Local models eliminate network round-trips. A voice message can be transcribed in milliseconds on a modern GPU, compared to seconds for a cloud API call.

### Natural Interaction

Voice transforms the agent from a text-based tool into a conversational partner. Users can interact while driving, cooking, or walking — no typing required.

---

## Interaction with Other Systems

- **Channel Integration**: Telegram voice messages auto-transcribe; Discord voice channels for real-time voice.
- **Multi-Runtime**: Transcribed voice is processed by whichever runtime the agent is configured to use.
- **Memory System**: Voice interactions are stored as episodic memories (transcription + metadata).
- **Confidence Router**: Voice queries are routed like any other query — simple greetings go local, complex questions go to cloud.
- **Inference Engine**: Shares ONNX Runtime infrastructure for efficient local model execution.

---

## The Takeaway

Voice is the most natural human interface. DuDuClaw's voice pipeline makes it accessible without sacrificing privacy or breaking the budget — local-first ASR and TTS handle the common cases for free, cloud providers handle the edge cases, and LiveKit enables the future of multi-agent voice collaboration.
