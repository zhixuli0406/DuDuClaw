# ADR-007: CEO/Board ガバナンスモード

- Status: Accepted(opt-in、デフォルトで無効)
- Date: 2026-07-09
- Related: WP17(`commercial/docs/TODO-client-demo-gaps-2026-07-08.md`)、ADR-004、RFC-21

## Context

ターゲット顧客層は企業導入へとシフトした——デモ顧客は約 20 名規模のチームを
運用している。非エンジニア出身のオーナーにとって、「自分が Board(取締役会)で
あり、自分の AI スタッフは CEO に報告する」というのは、生の agent 設定よりも
自然なメンタルモデルである。Paperclip がこの構造の実例を示している:
pause/resume/terminate や予算設定が可能な Board、Board の承認を得るために戦略を
提案する CEO、そして段階的に下位へ配分される予算。単独ユーザーはこれらを
一切必要としない。

## Decision

opt-in の `[governance] board_mode`(デフォルト `false`)を追加する。オフの場合、
すべての経路は現状とビット単位で同一である。オンの場合、新しいエンティティ型を
発明するのではなく、このメタファーを既存のプリミティブにマッピングする:

- **Board** = Board 権限を持つ人間ユーザー(WP15 の最細粒度ユーザーおよび
  `rbac.rs` と整合)。ハード不変条件:**Board は常に人間であり、agent では
  決してない。**
- **CEO** = `reports_to` ツリーのルート agent(既存の概念)。
- **Initiative** = `kind = "initiative"` タグ付けされたトップレベルの Task Board
  タスク(加算的)。
- 結果を伴うすべての意思決定は、既存の `ApprovalBroker` + audit を経由する。
  `action_kind` 文字列は型付きの `ApprovalKind`(保存済み文字列と serde 互換)に
  集約され、自動化が安全な種類のみを自動決定できるようにする。`StrategicPlan` と
  `AgentHire` は Board の人間のみが決定できる。

新規構築よりも再利用を優先する:freeze = WP1 の `agent freeze`;承認 =
ApprovalBroker;予算 = `budget.rs` + WP14 インシデント UI;採用 = 既存の
`create_agent`;組織 = `reports_to`;タスク = Task Board。純粋に新規なのは、
戦略提案フロー、Board パネル、および会社レベルの予算レイヤーである。

## Fail-closed 不変条件(強制済み、unit test あり——`governance.rs` を参照)

- `can_decide(StrategicPlan|AgentHire, decider)` は、decider が Board 権限を持つ
  人間である場合にのみ true を返す。agent の identity は無条件に拒否され、
  監査ログに記録される。
- `can_create_initiative` は Board の人間のみに許可される。CEO agent は
  Initiative を*委任される*ことはできるが、自ら作成することはできない。
- `board_mode` が有効な状態では、いかなる agent(CEO を含む)も MCP/tool 経路
  経由で `[budget]` の値を編集できない——Board パネルの RPC だけが可能である——
  これにより agent が自分自身の上限を引き上げること(self-promotion)を防ぐ。
  `agent_may_edit_budget(board_mode)` は有効時に false を返す。

## Consequences

- 単独ユーザーは影響を受けない(opt-in、デフォルト無効;これは WP17 テスト
  スイートにおけるハードテスト項目である)。
- 型付きの `ApprovalKind` は WP8(skill 有効化)や WP16(チャネルボタン)にも
  恩恵をもたらす——1 つの enum が、自動化がどの種類に触れてよいかを判断する
  唯一の場所となる。
- 残りの統合作業(管理対象):CEO の戦略提案生成、Board のダッシュボードパネル、
  会社→agent への予算カスケード配線、`create_agent`/`spawn_agent` における
  AgentHire の二段階承認。これらはすべて、ここに記録された不変条件の上に
  構築される。
