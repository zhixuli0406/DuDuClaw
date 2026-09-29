# ブラウザ自動化と Computer Use

> 3グループの MCP ツール。素の HTTP フェッチから仮想デスクトップまで。選ぶのはエージェントで、自動ルーターは存在しません。

---

## 経緯についての注記

このページの以前の版は、L1 → L2 → L3 → L4 → L5 と自力でエスカレーションする**5層の自動ルーター**を説明していました。「L4 Sandbox Browser」という層も含めて。

そのルーター（`browser_router.rs`）は 2026-04 に書かれ、呼び出し元を一度も得られず、CHANGELOG にも一度も登場しないまま 2026-09 に削除されました。そこで説明されていた L4 という層は、そもそも独立した機能として存在したことがありません。

実際に出荷されているものはもっと単純です：エージェントが選ぶ3グループの MCP ツールと、ヘッドレスブラウジング用の任意の外部 MCP サーバー。コスト規律はルーティングエンジンではなく、エージェント自身の判断とツール説明から来ます。

---

## 実際に出荷されているもの

### L1 — `web_fetch_cached`

SSRF 保護・ディスクキャッシュ・レート制限付きの素の HTTP GET。ステータス、content type、本文（6万文字で切り詰め）を返します。`ttl_seconds` でキャッシュを制御（既定 86400）。

SSRF ゲート（`web_fetch::validate_url` ＋ `resolve_public_addrs`）は内部ホストとクラウドメタデータのエンドポイントを拒否し、リクエスト時点で DNS を再解決し、解決されたすべてのアドレスが公開アドレスであることを要求してから、そのリクエスト用にピン留めします。同じゲートが常駐センシングの `http_poll` / `websocket` ソースも守ります。

用途：ドキュメントのある API、JSON エンドポイント、生のバイト列だけ必要なサーバーレンダリングページ。

### L2 — `web_extract`

同じキャッシュ付き・SSRF 検証済みの経路で URL を取得し、CSS セレクタで要素を抽出します。出力形式は `text`（既定）、`html`、`json`（属性と子要素を含む構造化）。

用途：コンテンツが初期 HTML に入っている従来型のサーバーレンダリングサイト。

L1 も L2 も JavaScript を実行しません。SPA は空の外殻を返すだけです。

### L3 — Playwright または Browserbase（外部 MCP サーバーとして）

JavaScript レンダリングのページを扱うには、そのエージェント自身の `<agent_dir>/.mcp.json` にブラウザ MCP サーバーを登録します（`mcp_template.rs` が Playwright / Browserbase の設定を生成します）。これはエージェント単位の MCP サーバーで、グローバル登録される DuDuClaw MCP サーバーとは別物です。

DuDuClaw のバイナリには含まれず、L2 からここへのフォールバックもなく、勝手にインストールされることもありません。運用者が追加し、そのエージェントの `allowed_tools` / `denied_tools` が呼び出せるかを決めます。

### L5 — Computer Use

7つの MCP ツールがコンテナサンドボックス内の仮想ディスプレイを駆動します：`computer_screenshot`、`computer_click`、`computer_type`、`computer_key`、`computer_scroll`、`computer_session_start`、`computer_session_stop`。

ループ全体を所有するのは `computer_use_orchestrator` です——コンテナのライフサイクル → スクリーンショット → Claude のビジョン解析 → アクション → 繰り返し——そして進捗を発信元チャネルに報告します。コンテナイメージ（既定 `duduclaw-computer-use:latest`）、ディスプレイサイズ、ネットワークモードは設定可能で、パニックやタスクキャンセル時でもクリーンアップが保証されます。

用途：人がコンピューターの前でできること全般——ログイン、ドラッグ＆ドロップ、視覚的なパターン認識。同時に、圧倒的に最も遅く最も高コストな選択肢です。

---

## セキュリティ：デフォルト拒否

L2 より上のすべての層は `agent.toml` での明示的な承認を必要とします：

```toml
[capabilities]
computer_use = false        # 7つの computer_* ツール
browser_via_bash = false    # Bash ツール呼び出しからのブラウザ起動
allowed_tools = [...]       # 許可リスト
denied_tools = [...]        # 拒否リスト
```

- `computer_use = false`（既定）では、すべての `computer_*` MCP ツールが拒否を返します。判定は fail-closed で、ファイル不在・TOML 破損・キーの型違いはいずれも拒否です。
- `denied_tools` は CLI に `--disallowedTools` として渡されると**同時に** MCP ディスパッチャのフロントドアでも強制されるので、PTY プール経路でも制限が効きます。
- `browser_via_bash` はもう環境フラグを設定しません。`DUDUCLAW_BROWSER_VIA_BASH` を読んでいた `bash-gate.sh` の許可リストは、他のシェルフックとともに `ba015a48` で削除されました。ケイパビリティ自体は有効で、`disallowed_tools()` と `CapabilitiesConfig::sandbox_level()` に供給されます。後者は codex / gemini ランタイムが `ReadOnly` と `WorkspaceWrite` のどちらのサンドボックスにするかを決める根拠です。
- 死んだルーターのフィールドとしてのみ存在した3つの制限（信頼／ブロックドメイン、セッションあたりのページ上限、スクリーンショット監査、アクションごとの人間承認）は**どこにも実装されていません**。不可逆アクションの承認ゲートは `ApprovalBroker` と `agent.toml [capabilities] approval_required_tools` / `irreversible_tools` が担います。

---

## おおまかなコスト比較

| 層 | 起動 | メモリ | JS 実行 | コンテナ要否 |
|---|---|---|---|---|
| L1 `web_fetch_cached` | ~0 ms | ~1 MB | いいえ | 不要 |
| L2 `web_extract` | ~0 ms | ~5 MB | いいえ | 不要 |
| L3 Playwright / Browserbase MCP | 数秒 | 数百 MB | はい | 不要（外部プロセスまたはクラウド） |
| L5 `computer_*` | ~10 s | 500 MB+ | はい | 必要 |

L1 で答えが出る問いに L5 を持ち出すのが最も高くつく間違いで、それを避けるのはエージェントの責任です。プラットフォーム側に止める仕組みはありません。

---

## 他システムとの連携

- **コンテナサンドボックス** — L5 はエージェントのタスク実行を隔離するのと同じコンテナ基盤で動きます（`--network=none`、tmpfs、読み取り専用 rootfs）。
- **セキュリティ防御** — ケイパビリティの強制と監査証跡は [05-security-defense.md](05-security-defense.md) を参照。
- **常駐センシング** — `http_poll` / `websocket` のティックソースは L1 と同じ SSRF ゲートを共有します。[41-resident-sensing.md](41-resident-sensing.md) を参照。
- **監査ログ** — 拒否されたものも含め、すべての MCP ツール呼び出しが引数と結果をマスクした形で `tool_calls.jsonl` に残ります。

---

## まとめ

正直版はルーターの物語ほど格好良くありませんが、運用はしやすい：Web に触れる4つの方法、それぞれのコストとそれぞれのスイッチ、そして自分で選ばなければならないエージェント。ルーティングエンジンを削除した時点で、このページはそれを説明するのをやめる必要がありました。
