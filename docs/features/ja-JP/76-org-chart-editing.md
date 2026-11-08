# 組織図の編集（報告先と部門）

AI スタッフの報告先（`reports_to`）と所属部門は、誰が誰に仕事を委任できるかを決めます。正式な記録は `<home>/org.toml` で、どの AI スタッフも書き込めません。各スタッフの `agent.toml` にある同名の項目は表示用のコピーです。RM-027 では、ダッシュボードからこの 2 つを変更するための gateway API を追加しました。**組織図ページは現在も読み取り専用で、編集画面は今後のリリースで提供します。**

## 誰が変更できるか

| 役割 | 動作 |
|------|------|
| Admin | 変更は**すぐに反映**されます。ダッシュボードはまず変更前後の確認を表示し、そのとき表示した `base` を付けて送信します。その間に組織図が変わっていた場合、変更は拒否され何も書き込まれません。反映した変更はすべて `org_change_applied` として、変更前後の値と操作者付きで監査記録されます。 |
| Manager | 変更は**申請**になり、現在の Admin が承認センターで承認してから反映されます。申請できるのは、自分が仕事を任せられる AI スタッフ（Operator／Owner の割り当て、または Operator レベルのチーム権限）についてのみです。新しい上司も自分が閲覧できるスタッフである必要があります。最上位（上司なし）への移動は Admin が判断します。 |
| Employee | 組織図を変更できません。 |

ロールは呼び出しのたびにユーザーデータベースから読み直すため、降格は次のリクエストから反映されます。AI スタッフが自分の配置を変更できないことに変わりはありません（`org_field_guard`）。

## すべての変更に適用されるルール

`agents.update`、すべての `org.*` 呼び出し、そして Admin が申請を承認するときにも、同じチェック（`duduclaw_core::org_change::validate_org_change`）が実行されます。

- 対象のスタッフと新しい上司が存在すること。
- 自分自身に報告する設定はできません。
- 循環を作らないこと。チェックは表示用コピーではなく `org.toml` をたどるため、正式な記録にだけ存在する循環も検出します。
- 部門名が有効であること。部門が 1 つでもある場合、新しい部門は既存のものに限ります（スタッフが使用している、または共有 wiki／共有スキルの部門フォルダーがある。先に「部門」で作成してください）。部門を空にする変更は常に可能です。

`agents.update` も不正な `reports_to` を同じエラーで拒否するようになりました（以前はチェックせずに書き込んでいました）。部門の扱いは変わりません。

ダッシュボードの raw エディター（`config.raw.set`、Admin）も、`agent.toml` の編集で `[agent] reports_to` または `department` が変わる場合は同じ経路を通ります。グローバル組織ロックを取得し、正式な記録に対して木全体のチェック（存在・自分自身・循環）を行い、正式な記録を先に、ファイルを後に書き込みます。ファイルの書き込みに失敗した場合は正式な記録を元に戻します。チェックに失敗した場合、raw の保存は検証エラーとともに拒否され、何も書き込まれません。この 2 項目を変えない編集は正式な記録に触れません。

### 変更の書き込み方

- **木全体で 1 つのロック。** すべての組織の書き込みは、プロセス間で共通の 1 つのロック `<home>/locks/org-mutation.lock` を取得します。対象は `org.*` 呼び出し、申請の承認、`agents.update` の組織項目、MCP `agent_update` の組織パス、`duduclaw org sync` です。ロック内で、書き込み側は正式な記録を読み直し、`base` を比較し、木全体でルールをチェックしてからコミットします。逆向きの 2 つの移動を同時に行っても（A を B の下へ、B を A の下へ）、両方が通ることはありません。
- **正式な記録が使える状態であること。** `org.toml` が一度作成された後で、欠落している、読み取れない、TOML として不正、または新しい DuDuClaw が書いたものである場合、すべての組織変更は `org_authority_unavailable` で拒否されます。表示用コピーで代用することも、空の状態からファイルを再構築することもありません。読み取り専用のビュー（`org.placement`、`org.pending_changes`、組織図）は引き続き動作し、`org.placement` は問題を `authority_error` で報告します。他のスタッフ項目の保存も引き続き可能です。欠落または破損したファイルを再構築できるのは `duduclaw org sync` だけで、全体の再構築に限られます。破損した元ファイルは `org.toml.corrupt` として残ります。新しいスキーマのファイルは書き換えられません。
- **正式な記録が先、表示用コピーが後。** 表示用コピーを更新できなくても、変更は有効です。正しいのは正式な記録だからです。gateway は `org_change_mirror_drift` を記録し、`mirror_sync_failed` を返します。再試行するには同じ配置をもう一度適用してください。gateway は `status:"mirror_synced"` を返します。この状態で `duduclaw org sync` を実行**しないでください**。古い表示値が正式な記録にコピーされてしまいます。`agents.update` が `agent.toml` の書き込みに失敗した場合は、直前の正式な記録の項目を元に戻します。

### AI スタッフからの保護

`org.toml` に加え、AI スタッフは `<home>/locks/org-mutation.lock` と `<home>/org_change_journal/` 以下のすべてを、書き込み・占有・削除できません（ファイル書き込みガードとシェルガードの両方）。このロックファイルの名前を含むシェルコマンドは、`flock … sleep` のような読み取りに見えるものも含めてすべて拒否されます。ロックを占有されると、すべての組織変更が止まってしまうためです。

## 申請と承認

- 1 人の AI スタッフにつき、待機中の申請は 1 件までです。2 件目は `pending_exists`（待機中の申請 ID 付き）で拒否されます。
- 判断はダッシュボードでのみ行います。メッセージングアプリにはボタンのない通知だけが届き、アプリから送られた判断は拒否されます。
- 承認時、gateway はルールを再確認し、スタッフの配置が申請時のままかを確かめてから、正式な記録と表示用コピーを書き込みます。判断は書き込みが成功してからのみ記録します。待機中に配置が変わった場合や変更が無効になった場合、申請は**却下**され、何も書き込まれません。
- 変更を適用した後で判断の記録に失敗した場合、申請は待機のまま（`decision_unrecorded`）で、最終の監査行は書かれません。適用済みの結果は承認 ID とともに `<home>/org_change_journal/` に保存されます。もう一度承認すると元の承認が完了し、変更が 2 回適用されることはありません。適用済みの申請に対する却下は拒否されます（`already_applied`）。監査行とチャンネル通知の更新は、判断が記録されてから書き込まれます。
- 却下では何も書き込まれません。申請は 3 日で期限切れになり、期限切れは却下と同じ扱いです。
- 待機中の申請があるスタッフを Admin が直接変更すると、その申請は取り下げられます。承認がすでに適用済みで再試行待ちの申請はそのまま残ります。
- 各申請は申請者を `on_behalf_of` として記録します。`[approval] forbid_self_approval = true` の場合、Admin を含め誰も自分の申請を承認できません。申請に記録された申請者も確認されます。自分の申請を却下することは引き続き可能です。

監査イベント：`org_change_requested`、`org_change_applied`（`actor` 付き。申請由来の場合は `approved_by` と `proposer` も）、`org_change_rejected`（`why`：`rejected`、`expired`、`stale`、`invalid`、`malformed`）、`org_change_mirror_drift`、`org_change_mirror_resynced`、`org_change_decision_unrecorded`、`org_change_decision_mismatch`。

## Gateway RPC

4 つとも両エディションで利用できます（組織図は人ではなく AI スタッフ間の関係のため）。

| メソッド | 役割 | パラメーター | 結果 |
|----------|------|--------------|------|
| `org.placement` | Manager 以上 | `{agent_id}` | `{agent_id, placement:{reports_to, department}, source:"authority"\|"mirror", mirror_drift, base, authority_error}` |
| `org.apply_change` | Admin | `{agent_id, reports_to?, department?, expected_base}` | `{status:"applied", agent_id, before, after, base, registry_reloaded, withdrawn_pending:[id]}` |
| `org.propose_change` | Manager 以上 | `{agent_id, reports_to?, department?, reason?}` | `{status:"pending_approval", approval_id, agent_id, before, after, base, expires_at}` |
| `org.pending_changes` | Manager 以上 | `{agent_id?}` | `{count, pending:[{approval_id, agent_id, before, after, base, stale, proposer, proposer_email, reason, created_at, expires_at}]}` |

省略した項目は現在の値のままです。空文字列は最上位／部門なしを意味します。申請の判断には既存の `approvals.decide {id, approve, reason?}`（承認種別 `org_change`）を使います。ルールチェックのエラーは `{code, message}` で、`code` は `agent_not_found`、`self_report`、`target_not_found`、`cycle`、`invalid_department`、`unknown_department`、`no_change`、`root_requires_admin`、`pending_exists`、`stale_base`、`org_authority_unavailable`、`org_authority_write_failed`、`org_lock_unavailable`、`mirror_sync_failed`、`decision_unrecorded`、`already_applied` のいずれかです。`stale_base` のときは `current` と `current_base` も返します。権限がない場合は文字列 `permission denied` を返します。`org.pending_changes` は、Manager には自分が出した申請と、自分が再編できるスタッフの申請だけを表示します。Viewer の割り当てだけでは何も表示されません。

組織図による委任の制御については[委任の分離](37-delegation-isolation.md)を参照してください。
