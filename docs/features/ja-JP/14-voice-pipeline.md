# 音声パイプライン

> HTTP と Telegram の音声認識・音声合成で、プロバイダー設定を共有します。

---

## 経緯についての注記

2026-09 まで、このページは4コンポーネントのローカルファースト・パイプラインを説明していました：SenseVoice ONNX ＋ Deepgram の ASR、Silero VAD、`symphonia` による音声デコード、LiveKit のマルチエージェント音声ルーム。

リポジトリ全体を `sensevoice`・`deepgram`・`silero`・`symphonia`・`livekit` で grep しても、**コードも依存関係も出てきません**——出てくるのは `.cargo/audit.toml` の中で `livekit-api` を RUSTSEC 免除の根拠として挙げた1行のコメントだけで、その crate は `Cargo.lock` に存在しません。この4つは一度も出荷されていません。

以下が実際に存在するものです。

---

## 共通の音声設定

HTTP と Telegram の音声認識は `config.toml [voice] stt_*` を使用します。HTTP、Telegram の音声返信と `synthesize_speech` MCP ツールは `TtsRouter::for_home` を共有し、`inference.toml [voice] tts_provider` / `tts_voice` に従って音声を合成します。

### 経路1 — HTTP エンドポイント（ダッシュボード、WebChat）

| エンドポイント | 認証 | 挙動 |
|---|---|---|
| `POST /api/stt` | Bearer JWT | ボディ上限 10 MiB。ボディに触れる**前に** `config.toml [voice]` からプロバイダを解決し、未設定なら **501 で fail-closed**——推測も文字起こしの捏造もしません |
| `POST /api/tts` | Bearer JWT | 共通ルーターを使用。`edge-tts`、`piper`、`minimax`、`openai-tts` は指定したプロバイダーのみ、それ以外の有効な設定はローカル優先。`none` / `off` / `disabled` は 501 を返します |
| `GET`／`POST /api/voice/config` | Bearer JWT | `[voice]` の STT 設定を読み書き |

**STT プロバイダ**（`stt.rs`、実装は2つ）：

- `OpenAiCompatStt` — `POST {base_url}/audio/transcriptions`、bearer key 付きの multipart。OpenAI Whisper も Groq Whisper もこの形を話します。
- `CommandStt` — ローカルのサブプロセステンプレート（例：`whisper-cli`）。音声を一時ファイルに書き、コマンドに渡し、stdout から文字起こしを読み戻します。

```toml
[voice]
stt_provider  = "openai_compat"   # または "command"。未設定 ⇒ /api/stt は 501
stt_base_url  = "https://api.openai.com/v1"
stt_api_key   = "sk-..."          # または stt_api_key_enc（AES-256-GCM）
stt_model     = "whisper-1"
stt_command   = "whisper-cli -m /models/ggml-base.bin -f {audio} --output-txt --no-prints"
```

**TTS プロバイダ**（`tts.rs`、1つの `TtsProvider` トレイトの下に4実装）：

| プロバイダ | 場所 | 備考 |
|---|---|---|
| `PiperTtsProvider` | ローカル | モデルディレクトリの ONNX ボイス |
| `EdgeTtsProvider` | クラウド | 任意機能。別途インストールする `edge-tts` CLI。API キー不要 |
| `MiniMaxTts` | クラウド | T2A v2。テキストを解析して CJK かラテン系のボイスを選択 |
| `OpenAiTtsProvider` | クラウド | 文字単位課金 |

`TtsRouter` はローカル優先（Piper → edge-tts → MiniMax → OpenAI）、クラウド優先、単一プロバイダーのルーティングを備え、検出済みで製品ポリシーが許可するプロバイダーだけを使用します。

Edge TTS は任意機能で、`pipx install edge-tts` による別途インストールが必要です。更新には `pipx upgrade edge-tts` を使います。Microsoft の非公式エンドポイントを使用するため、動作しなくなる可能性があります。DuDuClaw は CLI を独立した子プロセスとして呼び出し、Python パッケージを同梱しません。`duduclaw doctor` はネットワークに接続せず、ローカルで利用可能なプロバイダーを表示します。

### 経路2 — Telegram の音声メッセージ

音声または音声ファイルのメッセージは `transcribe_voice` を起動します：bot が `getFile` を呼び、返ってきた `file_path` をトラバーサル検証（`..`、絶対パス、NUL、許可外文字）し、`content_length` と実バイト数の両方でサイズ上限を確認してダウンロードし、文字起こしします。

**v1.68.0 からは `stt::transcribe_channel_audio` を呼び、ダッシュボードの音声認識設定（`config.toml [voice] stt_*`、`/api/stt` と同じプロバイダ）を使います。** 音声認識プロバイダが未設定の場合だけ、環境変数 `OPENAI_API_KEY` の OpenAI Whisper にフォールバックします。設定済みのプロバイダが失敗した場合はエラーになり、フォールバックしません。言語は引き続き `zh` 固定です。LINE の音声メッセージも同じ経路です。v1.68.0 より前は、どちらのチャネルも常に環境変数キーの Whisper を使っていました。

Telegram の音声返信（チャットごとに `/voice` で切替）は共通 TTS ルーターにプレーンテキストを渡します。利用可能なプロバイダーがなければボタンを非表示にし、`/voice` はオフのままです。合成失敗時はテキストに戻し、セッション内の初回失敗だけ短い通知を添えます。音声アップロード失敗時のテキスト返信も維持します。

v1.67.1 から、「語音」タブには「語音回覆模式」（音声返信モード）、「語音辨識」（音声認識）、「語言」（言語）の3項目が表示されません。これらが書き込んでいた `voice_reply_enabled`、`asr_provider`、`asr_language` を gateway で読むコードはありません。`inference.toml` に既にある値はそのまま残り、タブの保存ではこれらのキーを送りません。v1.68.0 からは設定構造からもこれらのフィールドが削除され、`system.update_config` は受け取っても無視します。`/api/stt` の音声認識は同じタブの詳細カード（`config.toml [voice] stt_*`）で設定します。

---

## ローカル Whisper

`duduclaw-inference::whisper` には2つのモードがあります：`Api`（OpenAI Whisper API）と `Local { model_path }`（`whisper-rs` 経由の whisper.cpp）。ローカルモードは**既定ではない `whisper` Cargo feature** の後ろにあります——この crate の `default` は `["cpu"]` です——ので、素のリリースバイナリには API モードしかありません。

---

## Discord 音声

`discord_voice.rs` は**既定ではない `discord-voice` Cargo feature** の後ろで **Songbird** をラップします。gateway の `default` は `["dashboard", "desktop"]` なので、素のビルドには含まれません。その背後に VAD も ASR の配線も今日は存在せず、このモジュールが扱うのはチャンネル参加と音声再生です。

---

## ONNX 埋め込み

`duduclaw-inference` の `onnx` feature は `OnnxEmbeddingProvider`（ONNX Runtime ＋ WordPiece トークナイザ）を有効にし、`bge-small-zh` のようなモデルに使います。これは音声コンポーネントではなく、メモリシステムのセマンティック検索が使う共有インフラです（[10-cognitive-memory.md](10-cognitive-memory.md) 参照）。そしてこれも crate の default features には**含まれません**。

---

## 他システムとの連携

- **チャネル** — Telegram は音声・音声ファイルのメッセージを自動で文字起こしし、`/voice` がチャットごとに音声返信を切り替えます。他のチャネルに音声経路はありません。
- **マルチランタイム** — 文字起こしはただのテキストです。エージェントが使うランタイムがそのまま処理します。
- **メモリ** — 音声ターンは通常のエピソード記憶として保存されます（文字起こし＋メタデータ）。
- **匿名化** — `/api/stt` から入った文字起こしは他のテキストと同じで、同じ MCP 側の匿名化ルールを通ります。Telegram 経路は文字起こしを通常の返信経路に流します。

---

## まとめ

HTTP、Telegram の音声返信と MCP 合成ツールは TTS 設定を共有します。プロバイダーの検出はエンドポイントへの接続確認ではありません。Telegram では合成に失敗するとテキストに戻ります。
