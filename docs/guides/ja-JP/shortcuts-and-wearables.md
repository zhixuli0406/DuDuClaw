# 手首とウェアラブル：Shortcutsショートカット、AI録音ペンダントの連携

> 前提条件：`duduclaw http-server --bind 127.0.0.1:8765` がすでに起動しており、
> `duduclaw mcp issue-refresh-token` または `~/.duduclaw/mcp_keys.toml` からBearerキーを
> 取得済みであること。スマートフォン／ウェアラブルからこのアドレスに到達できる必要がある
> （家庭内ネットワークのIP、またはTailscale）。

## 1. iPhone／Apple Watchのショートカット（配布審査もアプリ提出も不要）

Shortcutsの「URLの内容を取得」アクションだけでDuDuClawのHTTP APIを直接呼び出せる。
ショートカットで「Apple Watchに表示」をオンにすれば、文字盤のコンプリケーションから
ワンタップで実行できる。

**ショートカットA：AIスタッフに質問する**（音声ディクテーション → 応答を通知で受け取る）
1. 「テキストを書き起こす」
2. 「URLの内容を取得」：POST `http://<gateway>:8765/mcp/v1/call`
   - ヘッダー：`Authorization: Bearer <key>`、`Content-Type: application/json`
   - ボディ（JSON）：
     ```json
     {"jsonrpc":"2.0","id":"1","method":"tools/call",
      "params":{"name":"send_message","arguments":{"content":"聽寫文字"}}}
     ```
3. 「通知を表示」で応答内容を受け取る。

**ショートカットB：記憶を1件保存する**：上記と同様だが、`params.name` を `memory_store` に
変更し、`arguments.content` に書き起こしたテキストを入れる。

> 手首の上での承認判断はさらに簡単だ。ショートカットを使う必要はなく、LINE/Telegramの
> 承認カードに直接**返信**して「同意する」（同意）または「拒否する」（拒絕）と送るだけでよい
> （4チャンネルに対応。詳細はCHANGELOGを参照）。

## 2. Beeリストバンド（Amazon）：設定だけで接続完了

Bee公式のCLIはそれ自体がMCPサーバーであるため、エージェントの外部MCPリストに組み込むだけで
よい。エージェントはあなたの会話サマリーを直接参照できるようになる：

```toml
# agent.toml
[[mcp.external]]
name = "bee"
command = "bee"          # Bee CLI（公式ドキュメントに従いインストール・ログイン）
args = ["mcp"]
# ツール面はデフォルトでdeny-by-default。必要なものだけ有効化する
allow = ["get_conversations", "get_todos"]
```

外部MCPのマウント方法の詳細は [mcp-bridge.md](mcp-bridge.md) を参照。

## 3. Omi／Plaud：webhookで記憶に直接流し込む

gatewayの `POST /ingest/transcript` は、ウェアラブルベンダーのwebhook向けに設計された薄い
アダプターであり、同じBearerキーと同じ `memory_store` 書き込みパイプラインを共有する
（scopeチェックと発信元バインディングは通常どおり適用される）。受け付けるフィールド：
`text`／`transcript`／`summary`／`segments[].text`。

- **Omi**：App → Developer → Integration webhookに
  `https://<あなたの公開エンドポイント>/ingest/transcript` を設定する（`Authorization:
  Bearer <key>` が必要な場合はheader設定で指定する。Omiのバージョンがカスタムヘッダーに
  対応していない場合は、前段にCloudflare Workerを挟んでヘッダーを補う）
- **Plaud**：Developer Platform → Webhooksで同じエンドポイントを指定する（署名付きwebhookの
  検証はリバースプロキシ層で行うか、Bearerキーをそのまま信頼する）
- 手動テスト：
  ```bash
  curl -X POST http://127.0.0.1:8765/ingest/transcript \
    -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
    -d '{"source":"plaud","summary":"客戶說週五前要報價"}'
  # → {"stored":true,...}
  ```

この経路で保存された記憶には `wearable,<source>` タグと外部由来の情報が付与される
（信頼上限はv1.41のorigin bindingに従うため、ウェアラブルの書き起こしが高信頼の事実として
扱われることはない）。

## セキュリティに関する補足

- このキーが許可するのはMCPの外部ホワイトリストツール（memory/wiki/send_message、計7個）
  のみである。漏えいした場合は `duduclaw mcp` で失効・再発行すること。
- `/ingest/transcript` を公開インターネットに露出させる前に、rate limit
  （60 req/min/key）とリバースプロキシ層の発信元制限を必ず確認すること。
