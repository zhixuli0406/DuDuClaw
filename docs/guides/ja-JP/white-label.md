# 販売代理店キーコンソール（ホワイトラベル機能は削除済み）

> **ホワイトラベル機能は 1.72.2 で削除されました。** 製品名、ロゴ、サブタイトル、アクセントカラー、会社情報、About HTML ブロック、署名済みブランディングバンドル、`branding.*` RPC はすべて存在せず、`branding.json`／`branding.bundle.json` は無視されます。どの dashboard も DuDuClaw の製品表示と固定の上流ベンダー表記（嘟嘟數位科技有限公司 / Dudu Technology Ltd.）を表示します。この表記はバイナリに組み込まれており、設定ファイルや RPC では変更できません。
>
> 総代理店向けビルドでは、About ページに総代理店情報のブロックを 1 つ追加できます。これはダッシュボードの設定ではなくビルド入力です。[distributor-notice.md](distributor-notice.md) を参照してください。

このページは現在、ベンダー側の販売代理店キーコンソールのみを説明します。

## ベンダー向け（販売代理店コンソール）

管理者専用の **/manage/distributors** ページで、リセラーを登録し、マシンにひも付いたOEMライセンスキーを発行します。

1. 発行者の署名鍵を設定します（ライセンスkeygenツールが出力するのと同じ、32バイトEd25519シード形式、base64、1行）：

   ```toml
   # ~/.duduclaw/config.toml
   [distributor]
   issuer_key_path = "/path/to/license-signing-v2.key"
   ```

   未設定の場合、発行は明示的なエラーとともに拒否され、コンソールにはセットアップ用のカードが代わりに表示されます。鍵の内容がログやレスポンスに現れることはありません。

2. 販売代理店を追加し、**キーを発行**：リセラーが `duduclaw license fingerprint` で取得したマシンフィンガープリントを貼り付け、期間を選び（デフォルト365日）、生成されたblobをコピーします。リセラーは `duduclaw license activate <blob>` でこれを有効化します。

   **フィンガープリントは 1.66.1 以降で取得してください。**
   - **macOS**：1.66.1 から `IOPlatformUUID` でハードウェアに紐づけます。macOS 26（Darwin 27）は権限のない通常プロセスに対し全インターフェースの MAC を `02:00:00:00:00:00` として隠すため、インターフェースの選択では解決できず、識別子そのものを変更しました。**既存の macOS 環境のフィンガープリントはすべて変わります。**
   - **Linux／Windows**：フィンガープリントは変わりません。選択ロジックが全ゼロ・プレースホルダ・マルチキャストを除外するだけで、最初のインターフェースが既に有効だったホストの値は 1.66.0 と完全に同一です。

   **マシン移行やアップグレード後**：新しいマシンで `duduclaw license fingerprint` を実行してその値で再発行を依頼するか、`duduclaw license rebind`（コントロールプレーンの `/v1/license/rebind` を呼び出して再署名）を実行します。旧フィンガープリントで発行されたライセンスは**引き続き有効**です（互換判定）。gateway はログに再発行を勧める WARN を残すだけです。`duduclaw license status` の `Fingerprint binding:` 行に `strong` または `legacy (re-issue recommended)` が表示されます。受け付けるフィンガープリント一覧は `duduclaw license fingerprint --all` で確認できます（1行目は常に強いフィンガープリント）。

   `fingerprint`／`status` が「No hardware identity is readable」と警告する場合、そのマシンにはプラットフォーム UUID も使用可能な NIC アドレスも存在せず（コンテナや制限環境）、フィンガープリントはホスト名のみに紐づきます。発行には使えますが、ハードウェアレベルのコピー防止にはなりません。

   **マシン移行や OS アップグレード後**：新しいマシンで `duduclaw license fingerprint` を実行し、その値で再発行を依頼してください。旧フィンガープリントで発行されたライセンスは 1.66.1 でも**引き続き有効**です（互換判定）。gateway はログに再発行を勧める WARN を残すだけです。`duduclaw license status` の `Fingerprint binding:` 行に `strong` または `legacy (re-issue recommended)` が表示されます。このマシンが受け付けるフィンガープリント一覧は `duduclaw license fingerprint --all` で確認できます（1行目は常に強いフィンガープリントなので、スクリプトは1行目を読めば十分です）。

   発行される各ライセンスは、記録される前にバイナリへ焼き込まれたv2公開鍵に対して自己検証されます。鍵ペアが一致しない場合は明確にエラーとなります。

3. **失効**は、ローカル台帳（`distributor.db`）内でそのキーを失効済みとしてマークし、セキュリティ監査ログにも記録します。すでに有効化済みのインスタンスへの伝播は、後述するphone-home更新と署名済みCRLを通じて行われます。UIはこの時間差を正直に示し、即時失効であるかのように装いません。

### 発行済みキーを有効に保つ（更新と失効）

発行者キーが設定されている場合、オーナーのgatewayは自身が署名したキー向けに軽量なコントロールプレーンも提供し、60日間のオフラインダウングレードに引っかからないようにし、失効も伝播させます。以下の2つの公開エンドポイントは `[distributor] issuer_key_path` によって自己ゲートされます（未設定なら `404`、つまり素のgatewayは何も公開しません）：

| エンドポイント | 用途 |
|---|---|
| `POST /v1/license/refresh` | 呼び出し元のライセンスを `last_phone_home = now` で再署名します。**期間を延長することは絶対にありません**。期限切れのキーは更新ではなく再発行によって更新されます。失効済みキーには `revoked`、フィンガープリント不一致や期限切れには `403` を返します。 |
| `GET /v1/license/crl` | 署名済みの証明書失効リスト（クライアントが検証するのと同じ正規化ペイロードに対するEd25519署名）で、失効済みのすべての `subscription_id` を列挙します。TTLは7日です。 |

リセラー側のインスタンスは**コード変更が一切不要**です。環境変数を1つ設定してオーナーのgatewayを指すだけです：

```bash
# リセラー側のDuDuClawインスタンス上で
export DUDUCLAW_CONTROL_URL=https://your-gateway.example.com
```

これを設定すると、リセラーのgatewayは自身の階層ごとのスケジュールでphone-homeを行い、CRLをポーリングします。その結果：

- **更新**によりライセンスは無期限に有効な状態を保てます（phone-homeが成功するたびに `last_phone_home` が再スタンプされます）。オーナー側のコンソールにも、各キーの**最終更新時刻**が「生存している」というシグナルとして表示されます。
- **失効**は、phone-homeの間隔以内（OEM階層ではおおむね1週間）に、また独立してCRLのポーリング間隔（24時間）以内に、どちらか早い方でリセラー側に届きます。

発行者キーが設定されている限り、販売代理店コンソールには **Endpoint active** バッジと、このセットアップ用スニペットが表示されます。

> 正直な前提条件：リセラーが `DUDUCLAW_CONTROL_URL` を設定**せず**（かつ到達可能なクラウドコントロールプレーンもない）場合、そのキーはphone-homeに60日間成功しないままだと、オープンソース階層へダウングレードされます。更新エンドポイントは、それを指すよう設定されたインスタンスに対してのみ、このダウングレードを取り除きます。

#### エンドポイントをキーに焼き込む（`[distributor] public_url`）

リセラーに `DUDUCLAW_CONTROL_URL` の設定すら不要にしたい場合は、オーナーのgatewayの外部到達可能なURLを一度だけ宣言します：

```toml
# ~/.duduclaw/config.toml（オーナー側インスタンス）
[distributor]
issuer_key_path = "/path/to/license-signing-v2.key"
public_url      = "https://your-gateway.example.com"
```

以降、`distributor.issue` はそのURLを、キー自身が保持する `control_url` として埋め込みます。販売代理店側のインスタンスは、次の順序でコントロールプレーンを解決します：`DUDUCLAW_CONTROL_URL` 環境変数 → キーの `control_url` → 組み込みのデフォルト値。つまり、`public_url` を設定した状態で発行されたキーは、**クライアント側の設定ゼロ**でphone-homeと更新を行い、`duduclaw license refresh` も環境変数なしで動作します。`control_url` は署名対象ペイロードの一部ではありません（改変するには0600権限の `license.json` へのローカル書き込みアクセスが必要で、更新レスポンス自体も毎回署名検証されます。最悪の場合でも「URLに到達できない」というだけで、現状と変わりません）。更新時には元の `control_url` が保持されます。

## RPC サーフェス

| メソッド | アクセス権限 |
|---|---|
| `about.get` | ログイン済みの任意のユーザー（固定の製品・ベンダー情報、バージョン、ライセンス階層。`distributor_notice` は総代理店向けビルドのみ） |
| `distributor.status/list/add/update/remove/issue/revoke/upgrade` | 管理者 |

## HTTPコントロールプレーン（オーナーゲートウェイ、発行者キーでゲート）

| エンドポイント | 用途 |
|---|---|
| `POST /v1/license/refresh` | ライセンスを再署名する（phone-home）。 |
| `GET /v1/license/crl` | 署名済み失効リスト。 |
