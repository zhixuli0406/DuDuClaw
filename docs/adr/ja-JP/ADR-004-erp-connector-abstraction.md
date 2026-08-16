# ADR-004: ERP コネクタ抽象化(`trait ErpConnector`)

- Status: Accepted
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

DuDuClaw の ERP ブリッジには現在、具象型 `struct OdooConnector`
(`crates/duduclaw-odoo/src/connector.rs:103`)がひとつあるだけで、抽象化レイヤーは
一切存在しない。この型は connect / execute_kw / search_read / create / write /
count / version / status という一連のメソッドを提供し、15 個の Odoo MCP ツール
(CRM / Sales / Inventory / Accounting、`crates/duduclaw-cli/src/mcp.rs` で
ディスパッチされる)はすべてこの型に直接ぶら下がっている。エージェント単位の
credential と scope の分離はすでに RFC-21 §2 で実装済みである:`AgentOdooConfig` /
`OdooConfigResolver`(`crates/duduclaw-odoo/src/agent_config.rs`)、
`(agent_id, profile)` をキーとするコネクションプール `OdooConnectorPool`
(`crates/duduclaw-cli/src/odoo_pool.rs:54`)、そして `Scope::OdooRead / OdooWrite /
OdooExecute`(`crates/duduclaw-cli/src/mcp_auth.rs:52`)。問題は、この分離機構が
Odoo 専用に作られたものであり、別の ERP に切り替えるたびに全体を作り直す必要が
あることだ。

顧客調査(§1)から明確なシグナルが得られている:Odoo のポジションは 15〜50 人規模の
企業に収まっており、大企業顧客は Odoo だけで運用するわけではない。SAP、ERPNext、
Twenty のようなシステムを同じ agent プラットフォームに取り込むには、ERP を
接続するたびに `OdooConnector` の中身をコピー&ペーストするのではなく、まず
アダプター層が必要になる。

このパターンはプロジェクト内ですでに半年間実運用されている。`duduclaw-llm` の
`trait ChatProvider`(`crates/duduclaw-llm/src/provider.rs:103`)は
`#[async_trait]` で `id()` / `complete()` / `stream()` という 3 つのメソッドを
定義し、その下に Anthropic / OpenAI / Gemini / OpenAI-compat という 4 つの実装が
ぶら下がっており、さらにデータ駆動な能力テーブルである `ModelRegistry` を持つ。
同一プラットフォームの LLM 側ではすでに「1 つの trait + N 個の provider +
registry」が長期にわたって保守可能であることが証明されている。ERP 側で
これを再発明する理由はない。

trait の完全な仕様(メソッドシグネチャ、`duduclaw-erp` スケルトン crate の分割、
ERPNext 実装の詳細)は、すでに `commercial/docs/TODO-feature-gaps-2026-07.md`
§1 で計画済みであり、それは調査の成果物である。本 ADR が行うのはただ一つ:
その計画を正式な決定に昇格させ、トレードオフを記録することである。仕様は
ここでは繰り返さない——実行時には両方の文書を同時に開いておくこと。

## Decision

`ChatProvider` のパターンに倣い、`trait ErpConnector` を切り出す:
`#[async_trait]` + 安定した `id()` + registry 化された能力宣言。trait の
メソッド:

- `id()` — 安定した connector id(`"odoo"` / `"erpnext"` / …)。
  `ChatProvider::id()` に対応する。
- `capabilities()` — サポートするモデル / アクション / webhook を宣言し、上位層が
  データ駆動でルーティングできるようにする。
- `search` / `read` / `create` / `update` — CRUD の 4 点セット。Odoo の既存の
  search_read / create / write に対応する。
- `execute` — 汎用アクション(Odoo の execute_kw や sale_confirm のような
  ビジネスアクションに対応)。
- `webhook_subscribe`(optional)— イベント購読。サポートしない connector は
  not-supported を返せばよく、すべての実装に強制はしない。

**Odoo が最初の実装となる**:`OdooConnector` を `impl ErpConnector` に変更するが、
挙動はゼロ変更。既存の 15 個の MCP ツールの出力は byte-compatible を保ち、
既存のテストをそのまま回帰網として使う。**ERPNext は 2 番目の実装であり**、
その役割はこの抽象化を検証することにある——2 社目が接続できて初めて trait が
正しく抽出されたと言える。実装が 1 つしかない trait は推測であり、2 つ揃って
初めて証拠になる。

**per-agent の credential / scope / audit の 3 点セットは trait 契約の一部であり、
Odoo 固有の特例ではない。** RFC-21 §2 の一式——`(agent_id, profile)` を
キーとするコネクションプール、`allowed_models` / `allowed_actions` フィルタ、
`profile` が監査ログに入る仕組み——を汎用型に引き上げる。どの
`ErpConnector` 実装も、共有された `ConnectorPool<C>` からコネクションを
取得し、同じ scope チェックと監査帰属を適用される。新しい connector は
分離機構をタダで手に入れられ、誰かが ERPNext を接続する際に権限分離を
忘れるということが起きなくなる。

**MCP ツール名は `erp_*` に統一する**(`erp_record_search` /
`erp_record_create` / `erp_record_update` / `erp_execute` …)。旧来の
`odoo_*` という名前は、1 回の非推奨サイクルの間だけエイリアスとして残す。
非推奨期間中は両方の名前が呼び出し可能であり、期間終了後に `odoo_*` を
削除する。これにより、既存の agent の prompt や skill が同一リリース内で
壊れることはない。

## Consequences

**得られるもの:** 2 社目(ERPNext)の接続がコピー&ペーストでなくなる;分離機構は
一度正しく書けば全 connector で共有される;大企業顧客に対して「抽象化層は
準備済みで、X 社が計画中」という明確なセールストークが持てる(
`docs/features/erp-support-matrix.md` を参照);MCP ツール名が `erp_*` に
収斂し、背後がどの ERP かを漏らさなくなる。

**支払うもの:** trait を抽出するのには即時のコストがかかる——`duduclaw-erp`
スケルトン crate を切り出し、Odoo を実装者に変え、15 個のツールの回帰を
全緑にする必要があり、これらすべては ERPNext が実際に着手されるまでは
外部から見える新機能を何も生み出さない。正直なトレードオフ:**今抽出するか、
2 社目が来てから抽出するか**。

我々は今抽出することを選んだ。理由は、顧客のコンテキストによって ERP 拡張の
優先度が今回のラウンドに引き上げられたこと、ERPNext がすでに backlog
(`IMPL-PLAN-remaining-gaps-2026-07.md` §E)に組み込まれていること、そして
trait と 2 番目の connector を同時に着地させることでしか抽象化が正しいか
どうかを検証できないことである。2 社目が着手する時点まで待って抽出すると、
時間的プレッシャーの下で「抽象化 + 新実装 + 回帰」を同時にやらされることに
なり、リスクが高くなる。その代償として、新機能を伴わない構造再編のみの
期間を受け入れ、Odoo の既存テストで回帰リスクを抑え込む。

**非推奨の互換性:** `odoo_*` エイリアスは 1 リリース後に削除する。削除前に
CHANGELOG で Deprecated と表記し、削除時に Removed と表記し、アップグレード
ガイダンスを guide に書く。

**前提が崩れた場合:** ERPNext を接続する過程で trait のサーフェスに漏れが
見つかった場合(例えば一部の ERP に Odoo にはない batch / transaction
セマンティクスがある場合)、このファイルをその場でメソッドリストを拡張するのではなく、
新しい ADR でこの決定を修訂する。
