# Voice Pipeline

> Speech recognition and synthesis for HTTP clients and Telegram, with shared provider settings.

---

## A note on history

Until 2026-09 this page described a four-component local-first pipeline: SenseVoice ONNX + Deepgram ASR, Silero VAD, `symphonia` audio decoding, and LiveKit multi-agent voice rooms.

A repo-wide grep for `sensevoice`, `deepgram`, `silero`, `symphonia` and `livekit` returns **no code and no dependency** — only a comment in `.cargo/audit.toml` that names `livekit-api` as the justification for a RUSTSEC exemption, for a crate `Cargo.lock` does not contain. None of those four ever shipped.

What follows is what actually exists.

---

## Shared voice settings

HTTP and Telegram use `config.toml [voice] stt_*` for speech recognition. HTTP, Telegram replies and the `synthesize_speech` MCP tool share `TtsRouter::for_home` and `inference.toml [voice] tts_provider` / `tts_voice` for synthesis.

### Path 1 — the HTTP endpoints (dashboard, WebChat)

| Endpoint | Auth | Behavior |
|---|---|---|
| `POST /api/stt` | Bearer JWT | 10 MiB body cap. Resolves the provider from `config.toml [voice]` **before** touching the body, and returns **501 fail-closed** when none is configured — it never guesses or fabricates a transcript |
| `POST /api/tts` | Bearer JWT | Uses the shared router: `kokoro`, `azure`, `piper`, `minimax` and `openai-tts` select only that provider; other enabled settings use local-first routing. `none` / `off` / `disabled` returns 501 |
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

**TTS**

| Provider | Location | Notes |
|---|---|---|
| `PiperTtsProvider` | Local | Installed ONNX voices |
| `KokoroTtsProvider` | Local | Kokoro-82M v1.1-zh, Apache-2.0, downloaded on request |
| `MiniMaxTts` | Cloud | T2A v2; CJK or Latin voice selection |
| `OpenAiTtsProvider` | Cloud | OpenAI TTS |
| `AzureTts` | Cloud | Official Azure AI Speech REST; Taiwanese Mandarin voices |

`auto` tries installed Piper → installed Kokoro → MiniMax → OpenAI → Azure, subject to product policy. Explicit providers use only that provider; `off` disables synthesis. `duduclaw doctor` lists available providers without making synthesis requests.

The `command` provider runs an Admin-configured external program; DuDuClaw ships no synthesis program. Set `tts_provider = "command"` and `tts_command = "/path/to/my-tts --in {input} --out {output}"` through Settings → Voice. The command string is split on whitespace into argv (no shell); `{input}` and `{output}` are individual argument substitutions. Text is written to `input.txt` in a private temporary directory, and the program must write audio bytes to `output.audio`. The command has a 60-second timeout and receives only `PATH`, `HOME`, `USERPROFILE`, `TEMP`, `TMP` and `SYSTEMROOT`. In `auto`, this provider is attempted after the existing providers.

Download Kokoro from **Settings → Voice** (about 134 MB, Apache-2.0). Assets and the shared ONNX Runtime are verified during installation; interrupted downloads resume. Weights are not bundled in the binary. The model lives under `<home>/models/kokoro-zh/`. Default voice: `zf_001`; alternatives: `zm_010` and `af_maple` for pure English. Chinese speech has a mainland accent; polyphonic words may be mispronounced. Traditional characters and numbers are normalized, but the pronunciation rules do not cover every word. Input is capped at 2,000 characters and split into chunks of at most 500 tokens, joined with 150 ms silence. Kokoro produces 24 kHz WAV; Telegram receives it as `reply.wav` through `sendAudio`.

Azure sends the text to Microsoft. Set `AZURE_SPEECH_KEY` and `AZURE_SPEECH_REGION` in the gateway environment. Default Chinese voice: `zh-TW-HsiaoChenNeural`; default English voice: `en-US-AriaNeural`. Azure returns MP3. `tts_voice` can select a supported provider voice.

Admin RPCs: `voice.kokoro_status` reports installation, missing files, size, runtime readiness, progress and errors; `voice.kokoro_install` starts an idempotent background install. The Voice download card polls the status and displays install errors.

### Path 2 — Telegram voice messages

A voice or audio message triggers `transcribe_voice`: the bot calls `getFile`, validates the returned `file_path` against traversal (`..`, absolute paths, NUL, non-allowlisted characters), downloads with a size cap checked both on `content_length` and on the actual bytes, and transcribes.

**Since v1.68.0 it calls `stt::transcribe_channel_audio`, which uses the dashboard's speech-to-text settings (`config.toml [voice] stt_*`, the same provider `/api/stt` uses).** Only when no STT provider is configured does it fall back to OpenAI Whisper with the environment variable `OPENAI_API_KEY`; a configured provider that fails is an error, with no fallback. The language is still fixed to `zh`. LINE audio messages take the same path. Before v1.68.0 both channels always used the environment-key Whisper path.

Voice replies (toggled per Telegram chat with `/voice`) use the shared TTS router and plain text. Without an available provider, the toggle button is hidden and `/voice` stays off. Synthesis failures fall back to text and add a short notice on the first failure per session; audio upload failures also retain the text fallback.

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

HTTP, Telegram voice replies and the MCP synthesis tool share TTS settings. A detected provider is not proof that its endpoint is reachable; failed synthesis falls back to text in Telegram.
