# ローカルプロキシ — アカウントプールを Aider / Cline / Codex に貸す

`duduclaw proxy` は localhost に OpenAI 互換の HTTP エンドポイントを立て、DuDuClaw がすでに管理しているアカウントプールへ転送します。OpenAI chat API を話せるツール（Aider、Cline、Continue、Codex、あるいは `curl` 一行）をそこに向けるだけで、設定済みのキーとクォータをそのまま使えます。資格情報をディスクにもう一部置く必要はありません。

---

## クイックスタート

```bash
duduclaw proxy --bind 127.0.0.1:8788
```

キー未設定の初回起動時は一時的な Bearer キーを表示し、恒久キーの設定先も案内します。あとはクライアントを向けるだけです。

```bash
curl http://127.0.0.1:8788/v1/chat/completions \
  -H "Authorization: Bearer $DUDUCLAW_PROXY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"anthropic/claude-sonnet-5","messages":[{"role":"user","content":"hi"}]}'
```

Aider:

```bash
export OPENAI_API_BASE=http://127.0.0.1:8788/v1
export OPENAI_API_KEY=$DUDUCLAW_PROXY_KEY
aider --model anthropic/claude-sonnet-5
```

---

## エンドポイント

| メソッド | パス | 認証 | 備考 |
|---|---|---|---|
| POST | `/v1/chat/completions` | Bearer | ストリーミング（SSE）とバッファ応答の両方 |
| GET | `/v1/models` | Bearer | 同梱のモデルカタログ |
| GET | `/healthz` | 不要 | 死活監視 |

---

## モデル名

`provider/model` 形式の接頭辞がプロバイダを決めます。

```
anthropic/claude-sonnet-5   → anthropic
openai/gpt-5.5              → openai
gemini/gemini-3-pro         → gemini
deepseek/deepseek-chat      → openai-compat プリセット
```

接頭辞のない裸の id は `--default-provider` にフォールバックし、その既定値は `anthropic` です。`--default-provider openai` で起動すれば `gpt-4o` は `openai/gpt-4o` に解決されます。この値に対応する `config.toml` のキーはなく、起動時のフラグのみです。

---

## 認証

Bearer キーは**常に必須**です。解決順序：

1. コマンドラインの `--key <値>`
2. 環境変数 `DUDUCLAW_PROXY_KEY`
3. `config.toml` の `[proxy] key`
4. いずれも未設定 ⇒ ランダムキーを生成して表示（プロセス終了で失効）

```toml
[proxy]
key = "ddk-proxy-…"
```

比較は定数時間で行います。既定のバインドは loopback です。ルーティング可能なアドレスにバインドすると、そのポートに到達できるすべての相手にアカウントプール全体を晒すことになるため、0.0.0.0 に直接開くのではなく Tailscale や SSH トンネルの背後に置いてください。

レート制限はクライアント IP 単位で、MCP HTTP サーバーと同じトークンバケットを使います。

---

## 既知の制限：サブスクリプションの OAuth シートは転送できません

アカウントローテータは 2 種類のアカウントを保持します。**API キー**アカウントと、**サブスクリプションの OAuth シート**（Claude Pro / Team / Max）です。このプロキシで転送できるのは API キーアカウントだけです。ローテータが OAuth シートを選んだ場合、空の補完を黙って返すのではなく、明示的に拒否します。

> 選定帳號 `<名前>`（OAuth）為訂閱制 OAuth seat，proxy 轉發需 API key 帳號（OAuth 轉發為 PENDING-LIVE）

したがって、このプロキシを使うならアカウントプールに API キーアカウントを最低 1 つ追加してください。サブスクリプションの転送は未実装で、実装されるまでこのページはそう書き続けます。

---

## 失敗時の挙動

全体がフェイルクローズドです。使えるアカウントが無い場合は zh-TW の理由を添えて `503` を返します。コーディングエージェントが答えとして受け取ってしまう空の補完は決して返しません。上流のエラーは最も近い OpenAI 互換のエラー形状にマッピングされます。

---

## 関連

- [デプロイガイド](../../guides/ja-JP/deployment-guide.md) — プロキシが借りるアカウントプールの設定
- [Remote MCP](../../guides/ja-JP/remote-mcp.md) — 逆方向：外部クライアントが DuDuClaw のモデルではなくツールを駆動する経路
