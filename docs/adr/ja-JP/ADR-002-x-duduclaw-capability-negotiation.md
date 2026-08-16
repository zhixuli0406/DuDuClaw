# ADR-002 — x-duduclaw ヘッダーのバージョニングと能力ネゴシエーション

**Status:** Implemented (2026-05-06)
**Sprint:** W22-P0
**Owner:** TL-DuDuClaw
**Implemented in:** `crates/duduclaw-cli/src/mcp_headers.rs` + `mcp_capability.rs`
**Last updated:** 2026-05-06

---

## Context

DuDuClaw は HTTP/SSE の MCP エンドポイント(`/mcp/v1/call`、`/mcp/v1/stream`)を
公開しており、外部の client(Claude Desktop、CI パイプライン、サードパーティ統合)
から利用されている。プラットフォームが新しい能力(A2A Bridge、Secret Manager、
署名付き agent card)を出荷し、破壊的変更を導入していく中で、client 側には
以下を行うための信頼できる、機械可読な手段が必要になる:

1. 話している相手のサーバー上でどの能力が利用可能かを**発見**すること。
2. 自分が必要とする能力を**宣言**し、サーバーがトークンを無駄にしたり中途半端な
   処理を行ったりする前に、互換性のないリクエストを早期に拒否できるようにする
   こと。
3. サーバーの HTTP API 互換性レベルを、DuDuClaw の SemVer リリース番号とは
   独立に**理解**すること。

正式な header プロトコルがなければ、能力の発見はドキュメントの陳腐化と、
client/server のバージョン差異をまたいで診断しづらい実行時の驚きへと
劣化してしまう。

---

## Decision

3 つの HTTP レスポンスヘッダーと 1 つの HTTP リクエストヘッダーを導入する。
これらは総称して**x-duduclaw header protocol**と呼ばれ、HTTP サーバー上の
すべてのリクエスト/レスポンスに適用される。

### §1 — ヘッダー定義

| Header | Direction | Description |
|--------|-----------|-------------|
| `x-duduclaw-version` | Response | HTTP API 互換バージョン(§4.3 を参照) |
| `x-duduclaw-capabilities` | Response | カンマ区切りの、有効化されたサーバー能力の一覧(§4.1 を参照) |
| `x-duduclaw-capabilities` | Request(optional) | このリクエストに対して client が宣言する必要な能力 |
| `x-duduclaw-missing-capabilities` | Response(422 のみ) | サーバーが満たせないリクエスト能力のサブセット |

### §2 — 不変条件:すべてのレスポンスにヘッダーを付与する

`x-duduclaw-version` と `x-duduclaw-capabilities` の両方は、エラーレスポンス
(4xx、5xx)を含む**すべての** HTTP レスポンスに付与されなければならない。
これは `inject_capability_headers` という Axum middleware によって強制されて
おり、この middleware は `negotiate_capabilities` が生成する 422 を含む
すべてのルートをラップしている。

### §3 — 能力ネゴシエーションプロトコル

#### §3.1 — Client 側(request)
client はリクエストに `x-duduclaw-capabilities` を含めることで、自分が
必要とする能力を宣言してもよい(MAY):

```
x-duduclaw-capabilities: memory/3,mcp/2
```

#### §3.2 — 寛容モード(Permissive Mode)
client がこのヘッダーを省略した場合(または空値/不正な値を送った場合)、
サーバーはこのリクエストを能力要件なしとして扱い、そのまま通過させる。
**不在 = 寛容であり、拒否ではない。**

#### §3.3 — 422 Unprocessable Entity
client がこのヘッダーを含んでおり、かつ宣言された要件のいずれかが満たせない
場合:

| Failure mode | Server response |
|---|---|
| 能力が存在しない、または `enabled: false` | 422 — `server_version: null` |
| 能力は存在するがメジャーバージョンが低すぎる | 422 — `server_version: <current_v>` |

422 の body:
```json
{
  "error": "capability_mismatch",
  "message": "Required capabilities not available on this server",
  "missing": [
    { "capability": "a2a", "required_version": 1, "server_version": null },
    { "capability": "mcp", "required_version": 5, "server_version": 2 }
  ]
}
```

422 レスポンスは同時に `x-duduclaw-missing-capabilities: a2a/1,mcp/5` を
持ち、body をデシリアライズしなくても機械的に容易にパースできる。

### §4 — フォーマット仕様

#### §4.1 — x-duduclaw-capabilities のフォーマット
```
memory/<major>,<other-cap-alpha>/<major>,...
```

ルール:
1. `CAPABILITY_REGISTRY` 内で `enabled: true` になっている項目のみが現れる。
2. `memory` は**常に先頭**——これは DuDuClaw のコアとなる差別化要因である。
3. 他の有効化されたすべての能力は**辞書順(ASCII)**でそれに続く。
4. 各項目は `<name>/<major_version>` の形式で、スペースを含まない。
5. 項目はスペースなしでカンマ区切りされる。

例:`memory/3,audit/2,governance/1,mcp/2,skill/1,wiki/1`

#### §4.2 — メジャーバージョンのセマンティクス
capability registry 内の `major_version` は、その特定の能力に対して
**破壊的なプロトコル変更が生じたときにのみ**インクリメントされる。ある
能力の中で新しいオプションフィールドや新しいツールを追加することはメジャー
アップではない。`mcp/2` に固定された client は、すべての `mcp/2.x` の
挙動が安定していることに依存してよい。

#### §4.3 — x-duduclaw-version のセマンティクス
このバージョンは HTTP API の互換性を追跡するものであり、DuDuClaw の
SemVer リリース(`v1.11.x` など)とは独立している。HTTP API 自体に
互換性の変更(新しい必須ヘッダー、ステータスコードのセマンティクス変更
など)があったときにのみ変化する。

現在の値:`1.2`
- `1`:HTTP API が安定(beta を経て W20 で導入)
- `2`:2 回目の後方互換な HTTP 変更(W22——本 ADR、能力ネゴシエーションの
  追加)

### §5 — 能力登録簿(Capability Registry)

正規の registry は `crates/duduclaw-cli/src/mcp_headers.rs::CAPABILITY_REGISTRY`
にある。

| Capability | Major Version | Status | Sprint |
|---|---|---|---|
| `memory` | 3 | ✅ Enabled | core |
| `audit` | 2 | ✅ Enabled | W20-P1 |
| `governance` | 1 | ✅ Enabled | W19-P1 |
| `mcp` | 2 | ✅ Enabled | W20 HTTP/SSE Phase 2 |
| `skill` | 1 | ✅ Enabled | — |
| `wiki` | 1 | ✅ Enabled | — |
| `a2a` | 1 | 🔒 Disabled | W21(pending enablement) |
| `secret-manager` | 1 | 🔒 Disabled | W22 P0(pending) |
| `signed-card` | 1 | 🔒 Disabled | W22 P1(pending) |

無効化された能力は、送出される header に**決して**現れない。client が
無効化された能力を要求した場合、サーバーは `server_version: null` を伴う
422 を返す。

### §6 — Axum Middleware のレイヤー順序

```
router
    .layer(middleware::from_fn(negotiate_capabilities))    // INNER(リクエスト時に最初にチェックされる)
    .layer(middleware::from_fn(inject_capability_headers)) // OUTER(すべてのレスポンスにヘッダーが追加される)
```

Axum はレイヤーを登録の逆順で評価する(最後に呼ばれた `.layer()` が
最も外側になる)。この順序により、内側の `negotiate_capabilities`
middleware が生成する 422 を含め、`inject_capability_headers` が**すべての**
レスポンスに対して実行されることが保証される。

### §7 — 無効化された能力のポリシー

`enabled: false` の能力は、client から見ると未知の能力とまったく同じに
振る舞う:422 の body 内で `server_version: null` となる。これにより、
header プロトコルを通じて実装のロードマップ情報が漏れることを避けている。

無効化された能力は送出される `x-duduclaw-capabilities` ヘッダーから
省略されるため、機会的な発見(client がレスポンスヘッダーを読んで
自身を調整する)も正しく機能する。

### §8 — SDK とドキュメントの同期要件

capability registry に変更が生じた場合(能力の追加、有効化、または
メジャーバージョンの引き上げ):

1. `mcp_headers.rs` の `CAPABILITY_REGISTRY` を更新する
2. snapshot test `header_snapshot_matches_expected` を更新する——これは
   能力変更が静かに本番へ届いてしまう前の強制的な一時停止点として機能する
3. 本 ADR の §5 の表を更新する
4. CHANGELOG.md を更新する
5. 新しい能力が client 側の feature flag を必要とする場合、SDK
   メンテナーに通知する

---

## Consequences

### Positive(得られるもの)

- **ゼロコストの発見**:すべてのレスポンスがすでに能力メタデータを
  運んでいる——追加の往復は不要。
- **明示的な契約**:要件を宣言した client は、無言の部分的失敗ではなく、
  422 + 診断情報を受け取る。
- **デフォルトで加算的**:リクエストヘッダーを送らない client は、
  新しい能力の出荷によって壊れることがない。
- **テストで固定された registry**:snapshot test が、registry への
  偶発的な変更が静かに本番へ到達することを防ぐ。

### Negative / Trade-offs(取捨選択)

- **リクエストごとのオーバーヘッド**:`build_capabilities_header()` は
  すべてのレスポンスで `CAPABILITY_REGISTRY`(現在 9 項目)を反復処理する。
  現在の規模では許容範囲だが、registry が 100 項目を超えて成長する場合は、
  静的な `OnceLock<String>` として事前計算することを検討する。
- **メジャーバージョンのみのネゴシエーション**:client は細粒度の
  minor/patch 要件を表現できない。これは意図的な設計である——
  minor/patch の変更は定義上、常に後方互換である。

---

## Implementation

| File | Role |
|------|------|
| `crates/duduclaw-cli/src/mcp_headers.rs` | Registry、header builder、parser、ネゴシエーションロジック——23 個の unit test |
| `crates/duduclaw-cli/src/mcp_capability.rs` | Axum middleware(`inject_capability_headers`、`negotiate_capabilities`)——11 個の統合テスト |
| `crates/duduclaw-cli/src/mcp_http_server.rs` | 両方の middleware レイヤーを `build_router()` に組み込む |
| `crates/duduclaw-cli/src/lib.rs` | `pub mod mcp_headers` + `pub mod mcp_capability` をエクスポートする |

**Test coverage:** 34 個のテスト(unit テスト + `tower::ServiceExt::oneshot`
経由の Axum 統合テスト)、すべて成功。

---

## Alternatives Considered

### Alt A — バージョン付き URL パスのみ(`/mcp/v2/call`)
却下:URL バージョニングは主要な API 改訂を扱えるが、細粒度の能力の
有無を表現できない。`/mcp/v2/call` に接続する client は、この特定の
デプロイメント上で `a2a` や `secret-manager` が利用可能かどうかを依然として
知ることができない。

### Alt B — 能力発見用エンドポイント(`GET /mcp/capabilities`)
却下:すべてのセッション開始前に追加の往復が必要になる。header ベースの
発見は、情報が既存のすべてのレスポンスに便乗するため、ゼロコストである。

### Alt C — JSON-RPC の `initialize` 結果に能力を含める
却下:HTTP レイヤーは JSON-RPC レイヤーの下に位置する。一部のルート
(例:`/healthz`)は JSON-RPC envelope を一切処理しない。ヘッダーこそが
HTTP レベルのプロトコルネゴシエーションが本来あるべき正しいレイヤーである。

---

*ADR written by TL-DuDuClaw | 2026-05-06*
