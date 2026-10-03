# Voice Pipeline

> Speech in, speech out — two paths that do not yet share a config, and four components this page used to claim that have no code.

---

## A note on history

Until 2026-09 this page described a four-component local-first pipeline: SenseVoice ONNX + Deepgram ASR, Silero VAD, `symphonia` audio decoding, and LiveKit multi-agent voice rooms.

A repo-wide grep for `sensevoice`, `deepgram`, `silero`, `symphonia` and `livekit` returns **no code and no dependency** — only a comment in `.cargo/audit.toml` that names `livekit-api` as the justification for a RUSTSEC exemption, for a crate `Cargo.lock` does not contain. None of those four ever shipped.

What follows is what actually exists.

---

## Two paths, two configurations

The most important thing to know: **the HTTP voice endpoints and the Telegram voice handler are wired separately.** Since v1.68.0 speech-to-text (`config.toml [voice] stt_*`) reaches both, plus LINE audio messages; text-to-speech (`inference.toml [voice] tts_*`) still reaches the HTTP endpoints only.

### Path 1 — the HTTP endpoints (dashboard, WebChat)

| Endpoint | Auth | Behavior |
|---|---|---|
| `POST /api/stt` | Bearer JWT | 10 MiB body cap. Resolves the provider from `config.toml [voice]` **before** touching the body, and returns **501 fail-closed** when none is configured — it never guesses or fabricates a transcript |
| `POST /api/tts` | Bearer JWT | Reads `inference.toml [voice] tts_provider` / `tts_voice` on each request and picks a `TtsRouter` strategy from them (`edge-tts` → edge only, `minimax` / `openai-tts` → cloud best, anything else → local first); `none` / `off` / `disabled` returns 501 |
| `GET`/`POST /api/voice/config` | Bearer JWT | Read/write the `[voice]` STT settings |

**STT providers** (`stt.rs`, two implementations):

- `OpenAiCompatStt` — `POST {base_url}/audio/transcriptions`, multipart with a bearer key. OpenAI Whisper and Groq Whisper both speak this shape.
- `CommandStt` — a local subprocess template (e.g. `whisper-cli`); the audio is written to a scratch temp file, the command is run, and the transcript is read from stdout.

```toml
[voice]
stt_provider  = "openai_compat"   # or "command"; unset ⇒ /api/stt returns 501
stt_base_url  = "https://api.openai.com/v1"
stt_api_key   = "sk-..."          # or stt_api_key_enc (AES-256-GCM)
stt_model     = "whisper-1"
stt_command   = "whisper-cli -m /models/ggml-base.bin -f {audio} --output-txt --no-prints"
```

**TTS providers** (`tts.rs`, four implementations behind one `TtsProvider` trait):

| Provider | Location | Notes |
|---|---|---|
| `PiperTtsProvider` | Local | ONNX voices from the models directory |
| `EdgeTtsProvider` | Cloud | Free, no API key |
| `MiniMaxTts` | Cloud | T2A v2; picks a CJK or Latin voice by analyzing the text |
| `OpenAiTtsProvider` | Cloud | Pay per character |

`TtsRouter` dispatches across them under one of three strategies: `LocalFirst` (local → edge → MiniMax → OpenAI), `EdgeOnly`, or `CloudBest`.

### Path 2 — Telegram voice messages

A voice or audio message triggers `transcribe_voice`: the bot calls `getFile`, validates the returned `file_path` against traversal (`..`, absolute paths, NUL, non-allowlisted characters), downloads with a size cap checked both on `content_length` and on the actual bytes, and transcribes.

**Since v1.68.0 it calls `stt::transcribe_channel_audio`, which uses the dashboard's speech-to-text settings (`config.toml [voice] stt_*`, the same provider `/api/stt` uses).** Only when no STT provider is configured does it fall back to OpenAI Whisper with the environment variable `OPENAI_API_KEY`; a configured provider that fails is an error, with no fallback. The language is still fixed to `zh`. LINE audio messages take the same path. Before v1.68.0 both channels always used the environment-key Whisper path. Voice replies (toggled per chat with `/voice`) construct `EdgeTtsProvider` directly, also hardcoded, with a text fallback when the audio upload fails.

So `inference.toml [voice] tts_provider` / `tts_voice`, which the dashboard's Voice tab writes, reach `POST /api/tts` only and **do not currently affect the Telegram path**. Changing them in the UI changes nothing for a Telegram voice message. This is a known gap, stated here rather than left for an operator to discover.

Since v1.67.1 the Voice tab no longer shows 語音回覆模式 (voice reply mode), 語音辨識 (speech recognition provider) or 語言 (language). They wrote `voice_reply_enabled`, `asr_provider` and `asr_language`, which nothing in the gateway reads. Values already in `inference.toml` stay there untouched; the tab no longer sends those keys, and since v1.68.0 the fields are gone from the configuration struct and `system.update_config` ignores them. Speech-to-text for `/api/stt` is configured in the tab's advanced card (`config.toml [voice] stt_*`).

---

## Local Whisper

`duduclaw-inference::whisper` carries two modes: `Api` (OpenAI Whisper API) and `Local { model_path }` (whisper.cpp through `whisper-rs`). The local mode sits behind the **non-default `whisper` Cargo feature** — the crate's `default` is `["cpu"]` — so a stock release binary has the API mode only.

---

## Discord voice

`discord_voice.rs` wraps **Songbird** behind the **non-default `discord-voice` Cargo feature**; the gateway's `default` is `["dashboard", "desktop"]`, so a stock build does not include it. There is no VAD and no ASR wiring behind it today — joining a channel and playing audio is what the module covers.

---

## ONNX embedding

The `onnx` feature of `duduclaw-inference` enables `OnnxEmbeddingProvider` (ONNX Runtime + a WordPiece tokenizer) for models such as `bge-small-zh`. It is not a voice component — it is shared infrastructure for the memory system's semantic search (see [10-cognitive-memory.md](10-cognitive-memory.md)) — and it is **not** in the crate's default features either.

---

## Interaction with other systems

- **Channels** — Telegram auto-transcribes voice and audio messages; `/voice` toggles spoken replies per chat. No other channel has a voice path.
- **Multi-runtime** — a transcript is ordinary text: whichever runtime the agent uses handles it.
- **Memory** — voice turns are stored as ordinary episodic memories (transcript plus metadata).
- **Redaction** — a transcript entering through `/api/stt` is text like any other and passes the same MCP-side redaction rules; the Telegram path feeds the transcript into the normal reply path.

---

## The takeaway

Voice works in two places, and the honest version says so: a fail-closed HTTP pair that the dashboard configures, and a Telegram handler whose voice replies still use a hardcoded provider (its transcription follows the dashboard since v1.68.0). Naming the second one is more useful than a diagram of a VAD that was never written.
