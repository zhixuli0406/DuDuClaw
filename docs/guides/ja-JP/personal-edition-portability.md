# Personal Edition のデータ可搬性：セルフホスト ↔ マネージドの相互移行

> 対象：DuDuClaw Personal Edition（個人版）。マネージドで提供されるPersonal Editionインスタンスと、
> 自分でホストするPersonal Editionは**まったく同じ成果物**である。そのため両者の間でデータは自由に
> 移動でき、ベンダーロックインは存在しない。

## なぜ可搬なのか

Personal Edition（`EditionProfile::Personal`）は**自己完結した単一所有者**のデプロイ単位である。
クラウドの「マネージド」提供は、単に「同じPersonal Editionを私たちのインフラ上で動かしているだけ」で
あり、コンテナに含まれるのは `DUDUCLAW_EDITION=personal` のみだ。つまり、あなたの全状態は
`~/.duduclaw/` という1つのディレクトリに集約されている：

| 内容 | パス |
|------|------|
| エージェント（SOUL.md / CLAUDE.md / agent.toml / .claude） | `~/.duduclaw/agents/` |
| 記憶（episodic / semantic SQLite + FTS5） | `~/.duduclaw/memory*.sqlite` |
| 設定 | `~/.duduclaw/config.toml`、`~/.duduclaw/inference.toml` |
| ライセンス | `~/.duduclaw/license.json` |
| タスク／自動化／イベント | `~/.duduclaw/*.jsonl`、`events.db` |
| 実行中の送信 lease（WAL／SHM を含む） | `~/.duduclaw/egress_leases.db`、`egress_leases.db-wal`、`egress_leases.db-shm`、`egress_leases.lock` |

## 今すぐ使える方法：手動移行（tar）

標準ツールだけで、今日からPersonal Editionの状態一式を移行できる：

```bash
# 1. 移行元（セルフホストまたはマネージドからエクスポートしたディレクトリ）をパッケージ化
tar -C "$HOME" -czf duduclaw-export.tar.gz .duduclaw

# 2. 移行先のマシンに転送して展開する（先にgatewayを停止しておくこと）
tar -C "$HOME" -xzf duduclaw-export.tar.gz

# 3. 起動する。Personal Editionは既存のエージェントと記憶をそのまま読み込む
duduclaw start
```

> マネージド利用の顧客はダッシュボードからエクスポートを申請すれば、同じ形式の `~/.duduclaw/`
> tarballを取得でき、展開すればそのままセルフホストに移行できる。逆方向も同様に可能だ。両者は
> 同一のPersonal Edition成果物であるため、**変換作業は一切不要**である。

## 移行時の注意点

- **ライセンス**：`license.json` はマシンフィンガープリント（macOS はハードウェア UUID、Linux／Windows は hostname + MAC）に紐づいている。
  マシンを変更してもPersonal Editionのコア機能はそのまま動作する（Apache 2.0）。Pro付加モジュール
  がある場合は、ライセンスの発行元が新しいマシンへ紐付け直す必要があります。
- **チャンネルトークン**：channel bot tokenは暗号化された設定内にあり、一緒に移行される。IPや
  ドメインを変更した際は、webhook URLの更新を忘れないこと。
- **EditionProfile**：すべてのビルドで検証済みライセンスがエディションの上限を決めます
  （Business、OEM、Partner、Self-Host Pro は `enterprise`）。有効なライセンスがなければ `personal` です。
  `DUDUCLAW_EDITION`（`personal` または `enterprise`）は組み込みホストの `GatewayConfig.edition` より優先されますが、
  どちらも上限を狭めることしかできません。昇格要求は無視され、入力元ごとにプロセス内で一度だけ警告します。
  Business を `personal` に制限できますが、個人ライセンスを `enterprise` に昇格させることはできません。
  Debug assertions が有効でも昇格は許可されず、テストでは一時的な署名済みライセンスを使います。専用テスト API は非デフォルトの `test-edition-override` feature と Debug assertions の両方を必要とし、release ビルドではこの feature を拒否します。ダッシュボードの切り替えスイッチはなく、
  `agent.toml [edition] profile` も読み取られません。セルフホストの企業版には対応する有効なライセンスが必要です。

## macOS のライセンス識別と再紐付け

macOS の新しいフィンガープリントは `macos-uuid-v2:<32 桁の16進数>` です。ホスト名を含めず、`IOPlatformUUID` から生成します。`duduclaw license fingerprint`、ダッシュボードの有効化カード、再紐付け要求ではこの値を使用します。有効化は署名済みライセンスのインストールであり、その操作自体が発行を要求するわけではありません。従来の「ホスト名 + UUID」の候補は検証でき、旧方式として表示されます。実際のグローバル管理ユニキャスト MAC 候補は引き続き Hardware として有料ライセンスのハードウェア紐付けに使用できます。ローカル管理、マルチキャスト、MAC なし、全ゼロ、共有 `02:00:00:00:00:00`、ブロードキャスト `FF:FF:FF:FF:FF:FF` の MAC 候補は Weak として移行時の識別にのみ使用します。一致しても OpenSource に隔離し、Admin に承認済み再紐付けが必要と表示します。プレースホルダー UUID も Weak です。ただし過去のホスト名は復元できません。旧ライセンスが一致しなくなった場合は発行元に連絡し、新形式への対応後に `duduclaw license rebind` を実行してください。

新形式へ再紐付けしたライセンスは旧版のプログラムでは検証できません。移行前にライセンスを読むすべてのプロセスを更新し、ダウングレードが必要な場合は発行元に互換形式への再紐付けを依頼してください。

UUID の読み取りは最大3回、各回のタイムアウトは5秒です。取得できなくても UUID に紐付いたライセンスを MAC 候補で許可しません。CLI とダッシュボードも、UUID を読み取れるまで新規発行用のフィンガープリントの提供を拒否します。UUID のファイルキャッシュは作成しません。所有者だけが読み書きできるファイルでも同じアカウントの別プロセスが変更でき、その UUID がこのマシンから取得された証拠にはならないためです。Linux／Windows は hostname + MAC の紐付けを維持します。実際のグローバル管理ユニキャスト MAC は引き続き Hardware として、移行なしで有料ライセンスを検証できます。インストール済みライセンスが無効ならコア機能は OpenSource として動作し、Admin のダッシュボード、`duduclaw license status`、`duduclaw doctor` に理由が表示されます。

**コントロールプレーンの互換性：** refresh の `machine_fingerprint` はインストール済みの紐付けを保持します。異なる macOS UUID を `new_fingerprint` として提案しますが、**アカウント所有者がコンソールで再紐付けを承認した後にのみ有効**です。rebind は `old_fingerprint`、`new_fingerprint` と新しい nonce を送信します。フィンガープリントは公開識別子であり、所有権の証明ではありません。検証済みの署名を持つ `status: "pending"` では元のライセンスを保持し、コンソール承認待ちと表示します。HTTP 202 にもこの署名済み本文が必要で、空の応答や検証できない応答は拒否します。Linux／Windows と unbound OEM の refresh 紐付けは変わりません。macOS UUID を取得できない場合、紐付けのあるライセンスの refresh／rebind は停止し、MAC への変更を提案しません。

active 応答はライセンスと応答の両方の署名、完全一致する nonce と元のサブスクリプション、元または提案した紐付け（明示的な rebind は提案した紐付けのみ）、期限、候補の強度、オフライン猶予、プランと配備方式を検証します。active、revoked、pending のすべてに v2 署名エンベロープが必要です。失効応答は署名を検証し、`effective_from` の時刻が到来している場合にのみ、失効マーカーを永続化してインストール済みライセンスを削除します。偽造された応答や古い応答でライセンスやマーカーは変更されません。

**発行側の応答署名契約：** 数値 `response_signature_version: 2` のみを受け付けます。バージョンの欠落、旧版、不正な形式、未知のバージョンは拒否します。本文には `status`、`reason`、RFC3339 形式の `effective_from`、`nonce`、`subscription_id`、`original_fingerprint`、`proposed_fingerprint`、`license_digest`、`public_key_id`、base64 Ed25519 `response_signature` を含めます。署名対象は空白なし UTF-8 JSON 配列 `["duduclaw:license-response:v2",2,status,reason,effective_from,nonce,subscription,original,proposed,license_digest]` です。提案がなければ、提案した紐付けには元の紐付けを使用します。active の `license_digest` は Rust のフィールド順序と省略規則でコンパクトにシリアライズした `License`（署名と `control_url` を含む）の小文字 SHA-256 です。応答とライセンスの `public_key_id` は同一の信頼済みキーである必要があります。pending／revoked のダイジェストは空文字列で、応答の `public_key_id` により信頼済みレジストリから公開鍵を選択します。参照シリアライザーは `duduclaw_license::refresh::response_payload` です。発行側はアカウント所有者を認証し、コンソール承認を待って紐付けを更新し、ライセンスと応答を署名します。クライアントを更新する前に、この契約に対応した発行側を配備してください。

Gateway と CLI の有効化、インポート、更新、再紐付け、失効処理は、プロセス間 advisory lock を共有します。ネットワーク要求の前にロック内でライセンスファイル全体と失効マーカーのダイジェストを取得し、ロックを解放します。応答後に再取得し、どちらかのダイジェストが変わっていれば書き込みを拒否します。Gateway はメモリ上の revision も確認します。書き込みには一意の一時ファイル、ファイル fsync、原子的 rename を使用し、Unix ではディレクトリも同期します。このロックは協調する製品プロセスを保護しますが、直接書き込み権限を持つプロセスは advisory lock を無視できます。Windows ネイティブでの永続化、発行側の実際の配備、コンソール承認、メール配信、顧客環境の移行は別途検証が必要です。ライセンスファイル自体の署名からは引き続き `control_url` を除外しますが、必須の応答署名にはこの URL が含まれます。外部通信には[固定ホストと DNS のポリシー](../../features/ja-JP/05-security-defense.md#ライセンスの紐付けと更新)も適用します。独自の発行元はライセンスに URL を含めていても、クライアント側で `DUDUCLAW_CONTROL_URL` を明示的に設定する必要があります。`new_fingerprint` を無視するサーバーでは旧紐付けを維持し、弱い紐付けは承認された移行が成功するまで OpenSource のままです。

## ロードマップ（計画中）

- ダッシュボードに「ワンクリックでデータをエクスポート」（一鍵匯出我的資料）ボタンを追加し、
  tarballを生成する。
- 起動時にマネージドからエクスポートされたtarballをワンクリックでインポートできるようにする。
- マネージド ↔ セルフホストのラウンドトリップ整合性を自動検証する。
