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
| `POST /api/tts` | Bearer JWT | 共通ルーターを使用。`kokoro`、`azure`、`piper`、`minimax`、`openai-tts` は指定したプロバイダーのみ、それ以外の有効な設定はローカル優先。`none` / `off` / `disabled` は 501 を返します |
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

**TTS**

| プロバイダー | 場所 | 説明 |
|---|---|---|
| `PiperTtsProvider` | ローカル | インストール済み ONNX 音声 |
| `KokoroTtsProvider` | ローカル | Kokoro-82M v1.1-zh、Apache-2.0、必要時にダウンロード |
| `MiniMaxTts` | クラウド | T2A v2、CJK またはラテン文字の音声を選択 |
| `OpenAiTtsProvider` | クラウド | OpenAI TTS |
| `AzureTts` | クラウド | 公式 Azure AI Speech REST、台湾の中国語音声 |

`auto` はインストール済み Piper → インストール済み Kokoro → MiniMax → OpenAI → Azure の順に試し、製品ポリシーを適用します。プロバイダーを指定した場合はそのプロバイダーのみ使用し、`off` は音声合成を無効にします。`duduclaw doctor` は合成リクエストを送らずに利用可能なプロバイダーを表示します。

`command` プロバイダーは Admin が設定した外部プログラムを実行します。DuDuClaw に合成プログラムは同梱されません。「設定 → 音声」で `tts_provider = "command"` と `tts_command = "/path/to/my-tts --in {input} --out {output}"` を設定してください。コマンド文字列は空白で argv に分割し、shell は使いません。`{input}` と `{output}` はそれぞれ1つの argv 項目としてパスに置換されます。テキストはプライベートな一時ディレクトリの `input.txt` に書き込まれ、プログラムは音声バイト列を `output.audio` に書き込みます。タイムアウトは60秒で、環境変数は `PATH`、`HOME`、`USERPROFILE`、`TEMP`、`TMP`、`SYSTEMROOT` のみです。`auto` では既存の他プロバイダーの後に試します。

**設定 → 音声** から Kokoro をダウンロードします（約 134 MB、Apache-2.0）。インストール時にファイルと共有 ONNX Runtime を検証し、中断したダウンロードは再開できます。モデルの重みはバイナリに含まれません。保存先は `<home>/models/kokoro-zh/`。既定音声は `zf_001`、ほかに `zm_010` と英語専用の `af_maple` を選べます。中国語は中国大陸のアクセントで、多音字を誤読する場合があります。繁体字と数値を正規化しますが、すべての単語の発音を網羅するものではありません。入力上限は 2,000 文字、各チャンクは最大 500 tokens、間に 150 ms の無音を追加します。Kokoro は 24 kHz WAV を出力し、Telegram には `sendAudio` で `reply.wav` を送ります。

Azure はテキストを Microsoft に送信します。gateway 環境に `AZURE_SPEECH_KEY` と `AZURE_SPEECH_REGION` を設定してください。中国語の既定音声は `zh-TW-HsiaoChenNeural`、英語は `en-US-AriaNeural` で、MP3 を返します。`tts_voice` でプロバイダーの対応音声を指定できます。

Admin 専用 RPC：`voice.kokoro_status` はインストール、欠落ファイル、サイズ、ランタイム、進捗、エラーを返します。`voice.kokoro_install` は冪等なバックグラウンドインストールを開始します。音声のダウンロードカードは状態をポーリングし、エラーを表示します。

### 経路2 — Telegram の音声メッセージ

音声または音声ファイルのメッセージは `transcribe_voice` を起動します：bot が `getFile` を呼び、返ってきた `file_path` をトラバーサル検証（`..`、絶対パス、NUL、許可外文字）し、`content_length` と実バイト数の両方でサイズ上限を確認してダウンロードし、文字起こしします。

**v1.68.0 からは `stt::transcribe_channel_audio` を呼び、ダッシュボードの音声認識設定（`config.toml [voice] stt_*`、`/api/stt` と同じプロバイダ）を使います。** 音声認識プロバイダが未設定の場合だけ、環境変数 `OPENAI_API_KEY` の OpenAI Whisper にフォールバックします。設定済みのプロバイダが失敗した場合はエラーになり、フォールバックしません。言語は引き続き `zh` 固定です。LINE の音声メッセージも同じ経路です。v1.68.0 より前は、どちらのチャネルも常に環境変数キーの Whisper を使っていました。

Telegram の音声返信（チャットごとに `/voice` で切替）は共通 TTS ルーターにプレーンテキストを渡します。利用可能なプロバイダーがなければボタンを非表示にし、`/voice` はオフのままです。合成失敗時はテキストに戻し、セッション内の初回失敗だけ短い通知を添えます。音声アップロード失敗時のテキスト返信も維持します。

v1.67.1 から、「語音」タブには「語音回覆模式」（音声返信モード）、「語音辨識」（音声認識）、「語言」（言語）の3項目が表示されません。これらが書き込んでいた `voice_reply_enabled`、`asr_provider`、`asr_language` を gateway で読むコードはありません。`inference.toml` に既にある値はそのまま残り、タブの保存ではこれらのキーを送りません。v1.68.0 からは設定構造からもこれらのフィールドが削除され、`system.update_config` は受け取っても無視します。`/api/stt` の音声認識は同じタブの詳細カード（`config.toml [voice] stt_*`）で設定します。

---

## ローカル Whisper

`duduclaw-inference::whisper` には2つのモードがあります：`Api`（OpenAI Whisper API）と `Local { model_path }`（`whisper-rs` 経由の whisper.cpp）。ローカルモードは**既定ではない `whisper` Cargo feature** の後ろにあります——この crate の `default` は `["cpu"]` です——ので、素のリリースバイナリには API モードしかありません。

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
