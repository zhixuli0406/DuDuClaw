# ネイティブ Linux の保護サービス

v1.80 の構成では、gateway、AI 社員、文書解析、特権更新を分離します。確認済みの社員には永続的な Linux ID を割り当て、実行ごとにプロセス、ファイルシステム、ネットワークの namespace を作成します。必要な隔離コンポーネントが利用できなければ社員の実行を拒否します。

**開発状況：** 署名検証後の未有効化世代の配置、インストーラーから移行への接続、最初の管理者設定、読み取り専用の導入検査、固定パスの協調削除を実装しました。完全な新規導入、署名済み世代の有効化、移行プラットフォーム、gateway の総合準備検査は未完了です。今回 root 実機での導入・削除は検証していません。サービスの `active` や世代の配置だけでは隔離の準備完了を示しません。[実装と検証記録](../../architecture/native-linux-isolation.md)を参照してください。

**現在インストールできる内容：** `service install --protected-linux` は spawnd、egress、独立した更新サービスをインストールせず、移行計画を作成する処理もありません。隔離構成の新規インストール手順が完成するまで（v1.82.0 予定）、保護サービスは下記「単一実行ファイルの保護サービス（現行版）」で説明する v1.80 の単一実行ファイル gateway サービスです。このガイドで説明する社員ごとの隔離は適用されません。

## 利用者にとっての変更

- 隔離が必須の場合、社員は gateway のアカウントで実行されません。同じ社員は同時に一つの仕事を実行し、追加の会話やスケジュールは待機します。
- 社員は自分の CLI ログイントークンを読めます。隔離された実行経路では他の社員の home や gateway の認証情報にアクセスできません。個人接続は所有者が一人の確認済み Personal 社員に限り、Shared 社員は個人認証情報を使えません。
- 外向き通信は専用プロキシを通り、プライベートネットワーク、ホストの loopback、クラウド metadata を拒否します。公開宛先を制限するドメイン許可リストはありません。プロキシ非対応のツールは接続に失敗します。
- 信頼できない文書は使い捨ての低権限ワーカーで解析します。ネットワークはなく、サイズ制限付きの入力をファイル記述子で受け取ります。
- 更新は独立した root サービスと署名済みコンポーネントマニフェストを使います。アーカイブの取得や要求の受理は更新完了を意味しません。
- v1.79.3 からの移行には新しい認証情報と管理者の確認が必要です。旧アカウント、紐付け、ポリシー、hook、MCP サーバー、スケジュールは、取り込んだだけでは有効になりません。

保護は kernel、systemd、gateway、起動サービス、trampoline、egress サービス、更新サービスが信頼できることを前提とします。VM レベルの敵対的マルチテナント隔離やホスト root の侵害への保護ではありません。同じ社員の永続ファイルは実行間で共有します。通常のユーザーインストールにはこの隔離保証がありません。

## 対象ホストと確認済みテスト

ネイティブ Linux、PID 1 としての systemd、統一 cgroup v2、初期 user namespace が必要です。コンテナ、ID 再マッピング／rootless namespace、cgroup v1／混在構成は拒否します。公式 CLI バイナリは glibc 2.31 以降を必要としますが、この最低条件だけでは隔離のサポートを示しません。

対象ディストリビューションは Ubuntu 22.04、Ubuntu 24.04、Debian 12 です。2026-11-05 の **パッケージ単位** root テスト記録は次のとおりです。

| ディストリビューション | テストした kernel | systemd | Landlock ABI | 結果 |
| --- | --- | --- | --- | --- |
| Ubuntu 22.04 | 5.15.0-1120-gcp | 249.11 | 1 | パッケージテスト 66/66 成功 |
| Ubuntu 22.04、GCP 既定／HWE | 6.8.0-1070-gcp | 249.11 | 4 | パッケージテスト 66/66 成功 |
| Ubuntu 24.04 | 6.8.0-1071-gcp | 255.4 | 4 | パッケージテスト 66/66 成功 |
| Ubuntu 24.04、GCP イメージ既定 | 7.0.0-1014-gcp | 255.4 | 8 | 非対応：trampoline の 16 件すべて拒否、他の 50 件は成功 |
| Debian 12 | 6.1.0-54-cloud-amd64 | 252.39 | 2 | パッケージテスト 66/66 成功 |

Kernel 7.0 は**現在受け入れられません**。Trampoline は 7 より大きい Landlock ABI を拒否します。ディストリビューションや GCP イメージの名前ではなく、実際に起動した kernel を確認してください。検証には受け入れ可能な kernel 構成を使い、Landlock の無効化や ABI 検査の緩和を行わないでください。上記は完全なインストール、実際のベンダー CLI、完全な移行、更新復旧の検証ではありません。

`getent`、`id`、`useradd`、util-linux、systemd ツール、curl などの信頼できるシステムツールが必要です。社員の loop ストレージには ext4 または XFS、対応する非 thin provisioning の下位デバイス構成、社員のクォータと TCB の予約容量が必要です。不明な構成や容量不足は準備完了を拒否します。外部 IAM／NSS は社員 UID 範囲を永続的に予約する必要があります。

## 単一実行ファイルの保護サービス（現行版）

隔離コンポーネントをインストールできるようになるまで（v1.82.0 予定）、`service install --protected-linux` は v1.80 の単一実行ファイル保護 gateway をインストールします。root 所有のプログラム `/opt/duduclaw/bin/duduclaw`、ランチャー `/usr/local/libexec/duduclaw/protected-launcher`、ユニット `duduclaw-gateway.service`、バージョン下限 `/var/lib/duduclaw-protected/version-floor` で構成されます。ホストに隔離構成がインストールされるまで（v1.82.0 予定）、AI 社員は v1.80 と同じく gateway のサービスアカウントで実行され、v1.80 の認証情報保護が適用されます。社員も v1.80 と同じくダッシュボードから直接作成できます。このガイドで説明する隔離実行（spawnd による起動、個人専用社員、社員配置の提案）は、隔離構成が実際にインストールされたホストでのみ有効になります。

**新しいホスト。** バージョン下限の記録がない場合、ホストに DuDuClaw がインストールされたことがないと確認できたときだけインストールを続けます。条件は、`duduclaw` アカウントがなく、次のパスがすべて存在しないことです：`/var/lib/duduclaw`、`/var/lib/duduclaw-protected/root.key`、`/var/lib/duduclaw-protected/version-floor`、`/var/lib/duduclaw-protected/version-pending`、`/opt/duduclaw/bin/duduclaw`、ランチャー、`/etc/systemd/system/duduclaw-gateway.service`、`/etc/duduclaw/migration-import.json`、`/var/lib/duduclaw-migrate`、`/var/lib/duduclaw-updated/state.json`、`/opt/duduclaw-isolated/current`。いずれかが存在するか確認できない場合、記録のない下限は v1.79.3 のインストールである可能性があるため、インストーラーは移行のメッセージを表示して拒否します。拒否を回避するためにこれらのパスを削除しないでください。データや鍵が含まれている場合があります。

`--migrate-from` を指定すると、インストーラーはアカウント作成などの変更を行う前に移行元ディレクトリを検査します。読み取り中に変化した移行元は拒否されるため、ユーザーモードの gateway を先に停止してください。アカウントを作成する前に、root だけがアクセスできるマーカー `/var/lib/duduclaw-protected/fresh-install-pending` を書き込みます。その後インストールが失敗しても、マーカーが改変されておらず、バージョン下限、ルート鍵、`/etc/duduclaw/migration-import.json`、`/var/lib/duduclaw-migrate`、`/var/lib/duduclaw-updated/state.json`、`/opt/duduclaw-isolated/current` がまだ存在しなければ、同じコマンドを再実行するとインストールを続行します。前回すでにデータをコピーしていた場合は `--force-migrate` を追加してください（前回のコピーはバックアップとして保持されます）。マーカーは下限の記録後、または `uninstall` で削除されます。

このマーカーは、このホストで新規インストールを開始したことをインストーラーが記録するファイルです。root 所有、権限 0600 の通常ファイルで、内容がインストーラーの書き込んだものと完全に一致している必要があります。マーカーが存在してもこの検査に通らない場合、インストーラーはホストを新しいホストとして扱わず、インストールを拒否します。root は `sudo ls -l /var/lib/duduclaw-protected/fresh-install-pending` で確認できます。ホストにバージョン下限もルート鍵もなく、新規インストールを行うつもりの場合に限り、root で削除してください（`sudo rm /var/lib/duduclaw-protected/fresh-install-pending`）。削除後も、インストーラーは上記の新しいホストの条件を適用します。

```sh
# 検証済みのプログラムを root 所有の場所から実行します（「インストールと準備完了の確認」を参照）。
sudo ./duduclaw service install --protected-linux --enable-now
```

**v1.80.x のホスト。** 記録された下限が 1.80.0 以上のホストは、v1.80 の更新方法を引き続き使います。

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux [--version X.Y.Z]
```

`--version` を省略すると最新版を使います。コマンドはまず設定済みの AI CLI を検査し（下記参照）、公式リリースをダウンロードしてチェックサム、署名、埋め込みバージョンを検証し、プログラムをアトミックに置き換えます。ユニットとランチャーの内容が異なる場合は更新し、自動復旧の設定を再確認し、新しい下限を記録して、実行中の gateway を再起動します。対象バージョンはコマンドを実行するプログラムより新しい必要があり、そのプログラムは記録済みの下限より古くてはいけません（古いユニットとランチャーを書き込むため）。インストール済みの `/opt/duduclaw/bin/duduclaw` を使ってください。検証済みのより新しいプログラムに移る場合は、そのプログラムの `sudo ./duduclaw service install --protected-linux` を実行してください。再インストールでデータとルート鍵を保持するのは、バージョン下限が存在する間だけです。`service uninstall --protected-linux` は下限を削除しますが、データ、ルート鍵、アカウントは保持するため、その後のインストールは下限が不明なホストとして拒否されます。

**ダッシュボード。** 独立した更新サービスがない場合、ダッシュボードから保護されたインストールを更新できません。画面にその旨と上記のコマンドを表示し、要求は送信しません。

独立した更新サービスがインストールを管理するようになると（`/var/lib/duduclaw-updated/state.json` と `/opt/duduclaw-isolated/current` の両方が存在）、`service upgrade --protected-linux` は `--version` が必須になり、そのサービスへ要求を送るだけになります。接続できない場合も、プログラムを自ら置き換えることはありません。

## インストールと準備完了の確認

管理者自身の端末で、AI セッションの外から操作します。x86_64 または aarch64 の完全なリリースを取得し、root 所有で全祖先が他のアカウントに書き込みを許さないステージング領域で検証してから root で実行してください。Linux アーカイブは個別のコンポーネントバイナリを含みます。`duduclaw` だけをコピーしても、この構成はインストールできません。

完全なインストールには次の状態が必要です。

| 項目 | 必要な状態 |
| --- | --- |
| システム ID | `duduclaw`、`duduclaw-runsup`、`duduclaw-egress`、`duduclaw-dl`、`duduclaw-dns`。`SYS_UID_MAX` 未満で DynamicUser／社員範囲外の静的 ID |
| 社員 ID | 永続的な登録、通常 70000–99999。社員の確認後にのみ割り当て |
| 信頼するプログラム | Gateway、spawnd、trampoline、egress／forwarder／DNS broker、更新サービス／固定ランチャー、解析器、インポーター、移行コマンド、MCP stdio ブリッジ。root 所有の実行ファイル |
| サービス | Gateway、spawnd socket／service、egress socket／service、任意の DNS broker、更新サービス、parse socket／template、TCB／社員 slice |
| 解析用 root | `/usr/lib/duduclaw/parse-root` と `/usr/lib/duduclaw/parse-root-tree`。root:root 0755、初期状態は空、ホストデータを置かない |
| 署名済み状態 | Bootstrap の信頼、互換性のある世代、runtime 登録、更新スロット／状態、移行／準備完了の連携 |

解析器は[インストール参考](../../../crates/duduclaw-parse-worker/README.md)に記載された既知の空マウントポイントを残す場合があります。予期しない内容は調査し、解析用 root を再帰的に削除しないでください。Spawnd が社員の SSH、cron、at、user manager を封鎖し、ホストの実効状態を検証します。検査に通すために制御を緩和しないでください。

インストーラーは固定パスの完全なコンポーネント群を検証してから、アカウント、ディレクトリ、unit を準備します。Linux 配布物には固定更新ランチャーも含まれ、オフラインツールは署名済み情報とファイルから bootstrap inbox を作成できます。独立に認証された `duduclaw-updated --bootstrap-stage` は、コンパイル時の root 指紋、root 閾値署名、target 署名、全ファイルのハッシュを検証し、**未有効化の世代**を配置します。`current` の選択、trust／state／slots の作成、旧版移行の二重署名検証、サービス有効化は行いません。完全な新規導入の手順はまだ提供していません。

`sudo duduclaw service readiness --protected-linux`（`ops` は同じ入口の別名）は、実行ファイル、システムアカウント、ディレクトリ権限、unit 本文、systemd の実際の属性を読み取り専用で検査し、個別に報告します。I1 の総合検査アダプターは未接続のため、`MissingComposite` が必ず拒否し、社員実行を許可しません。カスタム DNS drop-in は未対応で unit 検査を拒否します。すでに新方式で管理される世代の再インストールは、この読み取り専用検査のみを行い、実行ファイルや unit を書き換えません。

## v1.79.3 からの移行

旧バイナリをその場で交換したり、旧データベースを新しい home にコピーしたりしないでください。旧環境は社員と UID を共有するため、旧権限データは信頼できません。インストーラーは v1.79.3 から v1.80 への境界（旧バージョン下限が不明で、新しいホストとも確認できない場合を含む。上記の単一実行ファイルの節を参照）を G の移行処理へ渡し、root 所有の固定計画 `/var/lib/duduclaw-migrate/plan.json` を読みます。計画が欠落・不正なら旧サービス停止前に拒否し、旧方式の上書き更新には戻りません。

管理者の手順は次のとおりです。

1. 公式サイトと認証済みライセンス console の両方から、bootstrap 検証プログラムと移行パッケージの SHA-256、および root metadata の指紋を取得し、一致を確認します。root 所有の 0700 ディレクトリへファイルとして保存し、検証プログラムの実行前にシステムの `sha256sum -c` で照合します。旧アーカイブの署名だけでは新しい信頼を確立できません。ダウンロードを shell へパイプしないでください。
2. 検証済みインストーラーが bootstrap 署名とプラットフォームを確認し、root 所有の不変な移行計画を作成します。計画を編集して権限や成功証跡を作らないでください。
3. 移行プログラムは**旧サービスを停止する前に**プラットフォームを検証します。その後、旧起動経路の無効化、旧データの隔離、検証済み暗号化スナップショット、新しい鍵、空の取り込み専用 gateway を準備します。低権限インポーターは既知の内容を制限付きレコードへ変換し、権限データは無効な提案として保存します。
4. 隔離の検証後、インストーラーの設定手順で新しい管理者を作成します。旧 session、パスワード、MFA、CLI ログイン状態は復元されません。高リスク項目の確認前に MFA を登録します。
5. **システム → 移行の確認**で完全な値を確認し、承認／拒否、適用失敗の再試行、認証情報の交換を行います。社員 ID とストレージは確認後に割り当て、個人アカウントは新しい所有者として再接続します。
6. 実際の適用結果を読み戻し、未確認・未交換の項目がなくなって初めて完了します。`AwaitingAdministrator` は管理者待ちです。root 専用の復旧スナップショットは保存期限と復旧検証の条件を満たすまで保持します。

対応する移行元バージョン：会話履歴（`sessions.db`）、目標と予約割り当て（`tasks.db`）、スケジュールタスク（`cron_tasks.db`）は v1.79.3 または v1.80.x の home から取り込めます。各データベースは既知のバージョンのいずれかと完全に一致する必要があります。手作業で変更されたもの、途中までしか更新されていないもの、他のリリースのものは丸ごと拒否され、インポーターのログに最も近い既知バージョンと異なるテーブル・インデックス・トリガー（内容は含まない）が記録されます。外部 A2A クライアントが所有するタスクは取り込まず、スケジュールタスクの元の依頼者・作成者・代理人も引き継ぎません。v1.79.3 の定義はリリースノートから再構成したもので、v1.79.3 のインストールから取得したものではないため、実際の v1.79.3 home での検証が必要です。

完全に検証済みのインストールでは、既存 runner のインターフェースは次の形です。

```sh
sudo /usr/lib/duduclaw/current/duduclaw-migrate run --plan /var/lib/duduclaw-migrate/plan.json
sudo /usr/lib/duduclaw/current/duduclaw-migrate status --plan /var/lib/duduclaw-migrate/plan.json
```

これはインターフェース例であり、**端から端まで検証済みの配備手順ではありません**。検証済みインストーラーが固定計画と署名済みプラットフォームを用意する必要があり、完全な導入成果物は未完成です。`duduclaw-migration-platform` がなければ runner は停止前に拒否します。最初の管理者の発行は root 相手を認証する Unix socket と loopback 設定ページへ接続され、token は URL fragment で渡し、アカウントと新しいパスワードを一つのトランザクションで作成します。リモート管理者は該当ポートへの SSH トンネルが必要です。root 所有の `/etc/duduclaw/migration-import.json` は移行モードを永続化し、再起動で `/run` 情報が消えても通常の管理者初期化へ戻ることを防ぎます。これらの接続だけで完全な移行成功とは扱いません。`prepare --stop-after-keys` は意図的にオフライン状態を残すため、アップグレードの近道として使わないでください。

レビューページには有効な Admin session、パスワード、登録済み認証アプリの新しいコードが必要です。認可は session に紐付き、五分間有効です。高リスク項目は個別承認が必要で、一括承認は低リスクに限ります。承認と適用成功は別の状態です。未対応カテゴリはブロックしたまま、対応する手順で再入力します。旧保留承認が新しい認可に変わることはありません。

## サービスアカウント用の AI CLI をインストールする

サービスは `duduclaw` アカウントで動作し、`HOME=/var/lib/duduclaw`、`PATH=/usr/sbin:/usr/bin:/sbin:/bin` を使用します。AI CLI はこれらのシステム PATH ディレクトリのいずれかにインストールし、サービスアカウントが読み取り・実行できる必要があります。`ProtectHome=yes` は `/home`、`/root`、`/run/user` を隠します。root が `/home/operator/.local/bin/claude` を見つけてもサービスからは利用できません。システムディレクトリから隠されたホームへのシンボリックリンクも使えません。

Claude Code では、実行前に `<ver>` をインストールするバージョンに置き換えてください。

```sh
cd / && sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>
sudo -u duduclaw -H claude auth login
```

`-H` と `--prefix /usr` は両方必要です。省略すると、元ユーザーの `~/.npmrc` の prefix 設定により CLI が再びユーザーのホームにインストールされる場合があります。Codex、Grok、AGY、Gemini を設定している場合も、サービスから読める同じシステム PATH にインストールし、各 CLI の手順でサービスアカウントとしてログインしてください。

`service install --protected-linux` と `service upgrade --protected-linux` の最後に、設定された主要推論 runtime と各社員の `agent.toml` が選択する runtime を検査します。移行時は移行先の設定を読みます。サービスアカウントとサービス PATH で実行権限を確認し、隠されたパスを除外します。元ユーザーのホームにしかない CLI はサービスで利用できないと表示します。主要 runtime がない場合は警告を表示し、非ゼロで終了します。後で runtime を設定する場合に限り、どちらのコマンドにも `--allow-missing-runtime` を指定できます。このフラグは CLI のインストールやログインを行いません。インストール最後の検査が失敗しても、すでに行ったインストールは元に戻りません。`service upgrade --protected-linux` はリリースのダウンロードや更新要求の送信より前にこの検査を行い、失敗した場合は何も変更しません。

移行でコピーするのは `.duduclaw` のデータだけで、**AI CLI 本体やログイン認証情報は移行しません**。Claude があってもサービスアカウントが未ログインの場合、インストール時に上記のログインコマンドを表示します。元ユーザーのホームから認証情報をコピーせず、個別にログインしてください。gateway の起動時に設定された推論バイナリが見つからない場合は、修復手順を含む ERROR を記録します。runtime の状態と doctor にも保護サービス用の手順を表示します。

## 更新とアンインストール

独立した更新サービスはまだインストールできません（v1.82.0 予定）。現在のホストは上記の単一実行ファイルの節に従って更新してください。移行と更新サービスの完全な準備後、Admin はダッシュボードで安定版を要求するか、インストール済み CLI を使います。

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux --version X.Y.Z
```

更新境界を通るのはバージョンと要求 ID だけです。サービスは署名済み metadata、ハッシュ、プラットフォーム、期限、バージョン下限、通し番号を検証し、完全な世代へ切り替え、実際のバージョン／プロトコルを読み戻してから commit します。再起動後も同じ要求を追跡し、`committed` だけを成功とします。サービス、信頼、準備完了情報がなければ拒否し、旧インストーラーを呼びません。通常の更新では降格できず、緊急ロールバックには別の署名認可が必要です。

削除入口は `sudo duduclaw service uninstall --protected-linux` です。隔離環境では unit を `/var/lib/duduclaw-protected/uninstall-units` に保存し、永続的な `/etc` mask を作成して読み戻し、timer、socket、サービスを停止します。停止済み unit、既知の空 cgroup、サービス／社員プロセスがないこと、既知のストレージ root にマウントがないことを確認した後で、固定パスの実行ファイルだけを削除します。強制・lazy unmount は行いません。鍵、データ、システムアカウント、永続 UID 対応、更新状態／世代、バージョン下限は復旧用に保持し、mask は自動再起動を防ぐため残します。H の任意カスタムストレージ root アダプターは未接続のため、その構成での完全な削除検証ではありません。今回は root 実機で削除していません。旧 uninstaller や保存状態の削除で拒否を回避しないでください。

## 毎晩の自動更新

Timer はホストの現地時間 03:30 に独立した更新サービスへ要求し、最大 20 分のランダムな遅延を加えます。`Persistent=false` により、夜間に停止していた場合でも朝の起動時に補完実行しません。固定 helper は `/usr/libexec/duduclaw-updated-launch --check-latest` を呼び、`--dry-run` では候補だけを調べて Apply を送信しません。

候補検索は `duduclaw-dl` で実行します。未署名のリリース索引は非 draft の安定版候補を選ぶだけで、root サービスは署名済みマニフェストを検証します。要求側は取得したプログラムをインストールせず、旧方式へ戻りません。完全な更新サービスと信頼状態を導入・検証した後にのみ予定を有効化してください。開発中の helper を v1.79.3 にコピーしないでください。移動する `main` ではなく、レビュー済みの固定版 [timer インストーラー](../../../scripts/linux-protected/auto-upgrade/README.md)を使用します。

## 準備完了エラーの調査

次のコマンドはインストール済みコンポーネントの調査用であり、完全なインストールを認証するものではありません。

```sh
sudo duduclaw service readiness --protected-linux
sudo /usr/lib/duduclaw/duduclaw-spawnd --check
sudo systemctl status duduclaw-gateway.service duduclaw-spawnd.service duduclaw-egress.service duduclaw-updated.service
sudo journalctl -u duduclaw-gateway.service -u duduclaw-spawnd.service -u duduclaw-egress.service -u duduclaw-updated.service -n 100 --no-pager
```

| 症状 | 対応 |
| --- | --- |
| `UnsupportedKernel` | 起動 kernel と Landlock ABI を確認。ABI 8／kernel 7.0 は現在拒否するため、検査を維持する。 |
| 準備情報が欠落、古い、非互換 | 完全な世代、プロトコル、アカウント登録、全サービスを確認。`active` だけでは不十分。 |
| UID／アカウント／SSH／IAM 失敗 | 予約範囲の衝突、別名、補助グループ、ログイン／cron／at、linger を解決。外部 IAM の予約には管理者の証跡が必要。 |
| ストレージ／クォータ失敗 | 下位デバイス構成、強制クォータ、空き容量、ホスト予約容量を確認。ディスク使用量ポーリングで代替しない。 |
| DNS／egress 失敗 | root が選んだ resolved Varlink／DNS broker 経路を確認。egress に私設 resolver IP の例外を追加しない。 |
| 解析 root 失敗 | 両 root の所有者、モード、予期しない内容を調査。ホスト `/etc` やデータをコピーしない。 |
| 移行プラットフォーム／設定リンクなし | 既存環境を保持し、完全な検証済み連携を取得。旧権限データのコピーや移行ゲート迂回をしない。 |
| 確認／交換待ち | 移行の確認で対応項目の確認、再試行、新しい認証情報設定を完了。承認だけでは移行完了にならない。 |
| 更新受理済みだが未 committed | 永続的な要求状態と準備エラーを追跡し、手動バイナリ交換を並行実行しない。 |

保護された社員環境ではコンテナエンジンやホストハードウェア依存の機能を提供しません。作業再開前に全必須コンポーネントの検査が必要です。パッケージテストの成功や本ガイドを本番環境の受け入れ完了とは扱わないでください。

## リリース済み v1.80.x の単一実行ファイル保護サービス

以下の各節は、v1.80.x リリースの保護サービスを説明します。gateway は root がインストールした単一の実行ファイルで、`service upgrade --protected-linux` はプロセス内で検証して置き換えます。これらはそのリリースと、隔離アーキテクチャでも維持する認証情報保護の制御（エンジンソケットポリシーと自動復旧）の参照情報です。現行版でも、単一実行ファイルのインストールではこのプロセス内更新を使います（上記「単一実行ファイルの保護サービス」を参照）。独立した更新サービスが管理するインストールだけが更新要求を送信し、その場合 CLI が実行ファイルを置き換えることはありません。

公式 GitHub 更新は minisign の trusted comment `duduclaw v{version} {archive-file-name}` をプラットフォーム・アーキテクチャまで厳密に照合します。Pro 更新は `duduclaw-pro` と独自の固定公開鍵を使います。ペイロード署名とコメントを保護する global signature の両方が必要です。すべてのアーカイブ導入経路で、検証器が生成した `VerifiedReleaseIdentity` が必須となり、アーカイブのダイジェスト、版、ファイル名、署名済みコメントを結び付けます。整合性確認だけでは導入できません。置換前に実行ファイルの版も確認します。保護アップグレードでは root でダウンロードしたコードを実行せず、ビルド時の固定形式レコードを読みます。ヘッダー `NUL + DUDUCLAW_RELEASE_ID:V1:`、安定版 `X.Y.Z`、末尾 `NUL + DUDUCLAW_RELEASE_END + NUL` の完全なレコードが正確に一つ必要です。すべての `DUDUCLAW_RELEASE_ID` がこの形式に一致しなければなりません。欠落、不正、重複、競合、マーカーのない旧版は拒否します。署名前に実際のアーカイブ内の実行ファイルを解析し、tag と照合します。Pro のパッケージ作成にも適用します。通常更新は `version` 出力を確認します。不一致は置換および保護版の intent/floor 更新前に中止します。この検査には修正済みの更新プログラムが必要です。旧版の更新プログラムは管理者が確認した手動導入で先に更新してください。

Provider は `verify_release_archive_with_pubkey(bytes, checksum, signature, pinned_key, expected_version, expected_file)` の結果を `install_verified_binary(bytes, &identity)` に渡してください。期待する版とファイル名は選択したリリース情報から取得し、ダウンロードした署名から流用しないでください。旧来の一引数の導入 API は廃止しました。

## 自動更新（任意）

`scripts/linux-protected/auto-upgrade/` は v1.80.0 からソースに含まれる任意の夜間更新スクリプトです。既存の `service upgrade --protected-linux --version` だけを呼び出すため、v1.79.3 の保護インストールでも使えます。ホストの現地時刻で毎日 03:30 に、最大 20 分のランダムな遅延を加えて実行します。既存の署名検証付き更新コマンドで、新しい公式署名済みリリースだけをインストールし、稼働中の gateway を再起動します。作業のない時間に設定してください。`Persistent=false` により、朝の起動後に夜間の未実行分を補って営業時間中に再起動することはありません。このモードではダッシュボードから更新を実行できず、タイマーの有無にかかわらず新しいバージョンと手動の `sudo` コマンドを表示します。未導入時は手動更新のままです。

v1.79.3 以前のソースタグにはこのディレクトリがなく、v1.80.0 以降のリリースタグには含まれます。`v1.80.0` タグが指すコミットなど、固定の完全なコミット SHA（以下では `<commit>` と表記。確認したコミットに置き換えてください）から取得し、確認してください。移動する `main` から取得しないでください（[ソースの場所](https://github.com/zhixuli0406/DuDuClaw/tree/<commit>/scripts/linux-protected/auto-upgrade)）。Linux リリースアーカイブには `scripts/` がありません。4 個のスクリプト／ユニットファイルを確認してから、`/root/duduclaw-auto-upgrade` など root だけが書き込める場所にコピーします。ファイルとすべての親ディレクトリが root 管理であることを確認して実行します。

```sh
# Replace <commit> with the reviewed full commit SHA; obtain files from that SHA.
# Run from the reviewed checkout; the destination must not already exist.
sudo cp -r --no-preserve=ownership scripts/linux-protected/auto-upgrade /root/duduclaw-auto-upgrade
sudo chown -R root:root /root/duduclaw-auto-upgrade
sudo chmod -R go-w /root/duduclaw-auto-upgrade
sudo sh /root/duduclaw-auto-upgrade/install.sh
sudo /usr/local/sbin/duduclaw-auto-upgrade --dry-run
systemctl list-timers duduclaw-auto-upgrade.timer
sudo journalctl -u duduclaw-auto-upgrade.service
```
ユーザーが書き換えられるディレクトリから `install.sh` を root として直接実行しないでください。確認後からコピーまでに別のプロセスが置き換える可能性があるため、root 管理のコピーも照合してください。インストーラーはソースディレクトリと4個のファイルのシンボリックリンク、通常ファイル以外、root:root 以外の所有者、グループ／他ユーザーの書き込み権限を拒否します。解決済みディレクトリから `/` までの各親も確認し、sticky bit 付きの `/tmp` も拒否します。検証後、`/run` の root 専用 0700 一時ディレクトリへコピーし、通常ファイルかつ 1 byte～1 MiB であることを再確認してからコピーをインストールし、一時ファイルを削除します。権限検査は出所の証明にはなりません。固定コミットと root 管理のコピーを照合してください。ディレクトリ内の README も参照してください。

GNU coreutils（`sort -V` を含む）、curl、grep、sed、tr、systemd と、既存更新処理のホスト準備要件が必要です。root の `version-floor` を読み取り、ファイルがない、不正、または `version-pending` が存在する場合は更新せず失敗します。手動で確認してください。GitHub の直近 30 リリースから最大の純粋な `vX.Y.Z` タグを選び、`desktop-v*` と接尾辞付きタグを除外します。同じ版か古い版なら大きなアーカイブを取得せず正常終了します。API エラーや候補なしの場合も更新しません。全履歴は検索しないため、30 件に gateway リリースがなければ手動調査が必要です。

圧縮された 1 行 JSON と整形済み JSON に対応し、外側の配列と純粋なバージョンタグを確認します。JSON 文法全体と `prerelease` フラグは解析しません。`-rc` などの接尾辞は除外しますが、純粋な `vX.Y.Z` が prerelease 指定でも候補になります。未認証 API はドラフトを返しません。Linux アーカイブのアップロード完了前にリリースが公開されると、その夜の更新は失敗し、翌夜に再試行します。journal を確認してください。

API 応答は 8 MiB、タイムアウトは 60 秒です。古い curl の `--max-filesize` は既知の Content-Length のみに適用されるため、応答を私有 `/tmp` の 0700 ディレクトリ内の一時ファイルに保存し、解析前に実サイズも確認して終了時に削除します。`PrivateTmp=yes` が私有の書き込み可能な `/tmp` を提供します。ホストへの書き込みは `/opt/duduclaw` と `/var/lib/duduclaw-protected` に限定し、ダウンロード再試行とインストール／再起動のため起動タイムアウトを1時間にします。純粋な `vX.Y.Z` は GitHub で prerelease とされていても、新しく署名検証に成功すればインストールされます。信頼の基点はアーカイブの minisign 署名であり、バージョンの選択自体は署名されていません。

gateway とデータを残し、スケジュール、スクリプト、ユニットを削除するには以下を実行します。更新中の場合、タイマーだけを無効化して削除を拒否します。更新完了後に再実行してください。

```sh
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

## 保護範囲と制限

**機能制限：** 保護されたネイティブモードでは、`InaccessiblePaths` でコンテナエンジンを遮断し、`PrivateDevices` でデバイスを隔離します。そのため、コンテナを使うタスクサンドボックス、コンピューター操作、ハードウェアに依存する機能は利用できません。AI 社員がコンテナエンジンにアクセスできないようにするための意図的な制限です。

ランチャーは実行ファイルと全祖先ディレクトリ、自身の配置先が root 所有で他のユーザーに書き込みを許さないことを確認します。アカウントを検査して同 uid の残存プロセスを終了した後に鍵を開き、uid/gid を変更し、補助グループと全 capabilities を削除して `no_new_privs` を設定します。gateway は鍵を受け取る前に non-dumpable にし、systemd の親プロセス、正確な cgroup v2 行 `0::/system.slice/duduclaw-gateway.service`、実行ファイルとプロセスの権限、自身から鍵のパスを開けないことを検証します。ネイティブインストーラー、鍵を開く前のランチャー、Rust の検証はいずれも初期以外の user namespace を拒否します。`/run/.containerenv`、`/run/systemd/container`、PID 1 の環境内の `container=` も拒否理由です。root のインストーラーとランチャーは `/proc/1/environ` を読める必要があり、非特権 gateway は権限不足の場合だけその読み取りを省略できます。コンテナでは従来のコンテナ規則を適用し、モード変数でネイティブ保護を取得することはできません。

既定の `revoke` ポリシーでは、既知の Docker／Podman／containerd ソケットは存在しないか、アクセス不能でなければなりません。書き込みアクセス検査と非ブロッキング接続の両方が拒否される必要があります。`DOCKER_HOST` は設定しないでください。独自 slice、cgroup v1／混在方式、想定外の sudo 出力、結果不明の検査はすべて拒否します。理由は journal または認証情報カードに表示されます。権限を緩めて回避しないでください。

ユニットには次の設定も適用します。

```ini
InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes
SystemCallArchitectures=native
```

[systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) の仕様に従い、各 `-` 接頭辞は存在しないパスを無視します。一般的なエンジンの接続先を隠し、ハードウェアを隔離し、カーネル／モジュール／cgroup の変更と set-ID ファイルの作成を禁止して、実行モードと syscall アーキテクチャを制限します。`RestrictNamespaces` は設定しません。gateway の Codex／bwrap タスクサンドボックスは user、mount、PID、network namespace を必要とします。

起動時と 60 秒ごとに、一般的なエンジンソケットへの到達性と補助グループを検査します。既定の `revoke` ポリシーで定期検査に失敗すると、保持中の鍵をその場でゼロ化し、保護状態を拒否に変更して警告を記録します。個人接続は、新たな起動時の鍵受け取り検査に成功するまで停止します。失効後も監視を続けます。自動復旧は次の節を参照してください。`/proc/net/unix` の待ち受け中の abstract socket も検査します。`/` で区切られた名前の要素が `docker`、`containerd`、`podman`、`buildkit`、`lxd` と完全一致するか、その名前の直後に `.`、`-`、`_` が続く場合に候補とします。任意の部分文字列では一致させません。 名前の一致だけでは拒否しません。短いタイムアウトの非ブロッキング接続で `SO_PEERCRED` を取得し、接続できた相手の uid が 0 の場合だけリスクと判断します。到達不能な候補は無視します。表は生のバイト列として読み取り、非 UTF-8 名が他のソケットの検査を妨げないようにします。すべての行を完全に解析し、不正なフィールドや flags、曖昧な名前、改行による断片があれば鍵の受け取りを拒否し、保持中の鍵も失効させます。表の読み取り失敗はどの状態でも検査失敗とし、復旧時の連続成功回数もリセットします。安全と判断する前に、必須の `NETLINK_SOCK_DIAG` dump で長さ付きの `UNIX_DIAG_NAME` を調べます。非エンジン名も含めて改行を拒否するため、改行の後が正しい形式の偽造行でも見逃しません。dump が取得不能、不正、中断、8 MiB 超過、250 ms のタイムアウトの場合も拒否し、カーネルには UNIX socket diagnostics の対応が必要です。どちらの検査も定期的な観測であり、全ソケットの原子的なスナップショットではありません。

`@docker_cli_` と 32 桁の小文字 16 進数に完全一致し、接続できる peer uid 0 の abstract socket は、必ずエンジンソケットと判定します。既定の `revoke` は鍵を失効させますが、管理者が選択した `warn` は鍵を保持して警告を表示し続けます。名前自体による免除はありません。名前の識別は診断のヒントにだけ使用します。通常は、このホストで誰かが root として `docker` コマンド（例：`sudo docker compose …`）を実行したことを示します。`revoke` では、実行中は個人アカウントを一時停止し、終了後に環境チェックが成功して自動復旧が利用できれば、通常は数分で復旧します。回避するには docker グループの一般ユーザーで docker を実行するか、開発用コンテナを別のホストで実行してください。

AI 社員の Bash／ツールは gateway と同じ uid で動作します。保護は non-dumpable、root 所有の鍵とプログラム、sudo／linger がないこと、エンジンソケットへのアクセス拒否に依存します。社員は書き込み可能なデータを変更し、gateway を停止し、許可された操作を要求し、自分のターンで発行されたアクセストークンを読めます。ホストは `duduclaw` アカウントにコンテナエンジンや他の特権ブローカーへのアクセスを与えてはいけません。起動時と毎分の検査は一般的な接続先を対象とし、無条件の隔離を保証しません。未識別名の abstract socket、別パスのエンジンソケット、特権ブローカー、その他の権限昇格経路、ホスト root の侵害、gateway 内ですでに動いている悪意あるコードは対象外です。検査の間に新しいアクセス経路が生じる可能性もあります。実行ファイルの所有者検査はサービス uid による改変を防ぐもので、悪意ある root 管理者が渡すファイルの署名検証ではありません。

この鍵は汎用サンドボックスではありません。鍵を保持する間は gateway 内の ONNX 読み込み禁止も適用され、NER マスキング、ローカル embedding、Kokoro は使えません。必要に応じて正規表現のマスキングを使用してください。データとルートキーの両方を含むバックアップは認証情報を復号できます。ダッシュボードの保護キーだけを替えても旧データの復元は防げません。漏えいが疑われる場合、ホスト管理者はサービスを停止し、安全にルートキーを交換し、保護の再設定とアカウントの再接続を行ってください。

ネイティブ systemd のインストール、sudo の各バージョン、ソケット検査は対象ディストリビューションでの実測が必要です。単体テストや macOS のコンパイル成功を Linux 実機の検証と同一視しないでください。

## コンテナエンジンのポリシーを選ぶ

既定の `engine_socket_action = "revoke"` は、エンジン制御インターフェースにアクセスできる場合、鍵の受け取りを拒否するか、保持中の鍵を失効させます。開発用コンテナと同じホストでサービスを維持する必要がある場合、ホスト管理者は `warn` を選択できます。例外はネイティブ Linux の確認済み `engine_socket_reachable` に限り、起動時の鍵受け取りと 60 秒ごとの検査に適用します。補助グループ、実行ファイルの権限、cgroup／unit、user namespace、コンテナ環境、鍵パスへのアクセス、ソケット表の読み取り失敗や不正形式、診断の失敗など、ほかの問題では引き続き拒否・失効します。systemd の隔離設定は変わらず、コンテナに依存する機能も利用可能にはなりません。

「警告のみ」に変更すると、ホスト上のプログラムがコンテナエンジンの制御インターフェースを公開しても、DuDuClaw は Google などの個人アカウントを停止しません。コンテナエンジンを操作できるプログラムは、実質的にホスト全体の最高権限を持ちます。AI 社員がアクセスできれば、復号鍵を読み取れる可能性があります。AI 社員がエンジンにアクセスできないことを確認でき、同じホストでコンテナを実行する必要がある場合に限り使用してください。開発用コンテナを別のホストに移すか、docker グループの一般ユーザーで docker を実行する方が安全です。

AI セッションの外で、自分の端末から実行してください。検証済みの root 所有バイナリを使用し、root の PATH に `duduclaw` がない場合は `/opt/duduclaw/bin/duduclaw` に置き換えます。

```sh
sudo duduclaw service protected-linux policy show
sudo duduclaw service protected-linux policy engine-socket warn --yes
sudo duduclaw service protected-linux policy engine-socket revoke
```

コマンドは root を要求し、AI セッションでは拒否します。`warn` への変更時はリスクを表示し、`--yes` が必要です。唯一のポリシー元は `/etc/duduclaw/protected-policy.toml` で、サービスアカウントの `config.toml` は読みません。gateway は鍵の受け取り時と各定期判定前に、固定したディレクトリとファイルの記述子で読み取ります。root 所有の通常ファイル、ハードリンク数 1、サイズ制限、厳密な構文、root 所有の親ディレクトリを要求し、ファイルと親ディレクトリのグループ／他ユーザーによる書き込みを拒否します。欠落、読み取り失敗、不正なポリシーは `revoke` に戻ります。root コマンドは一時ファイルを書いて同期し、原子的に名前を変更してからディレクトリを同期します。

ポリシー変更は root 所有の `/etc/duduclaw/protected-policy-audit.jsonl` に追記します。先に操作意図を同期し、ポリシーを原子的に書き込んでから変更を記録します。Gateway は次の読み取り時に安全監査インデックスへ取り込みます。`warn` でエンジンへのアクセスを検出すると、検査の詳細を含む WARN を記録し、状況が続く間、認証情報保護パネルに警告を表示します。警告の開始と終了で各 1 件の監査イベントを記録し、同じ状態を 60 秒ごとに繰り返し記録しません。パネルは現在のポリシー、リスク、切り替えコマンドを表示するだけで、変更ボタンはありません。変更は次の検査で反映します。すでに失効した鍵は、次節の復旧処理で root ランチャーから再度受け取る必要があります。

警告の遷移は `<home>/state/engine-socket-warning-events.json` に同期保存し、両方の監査記録の取り込み成功後に確認して削除します。このサービス所有の日誌は診断専用で、ポリシーの権限を与えません。root の監査ログは 4 MiB が上限です。上限に達すると管理者がログを保管するまで変更を拒否します。既存のポリシーディレクトリはサービスが読み取り・通過できる必要があります。権限が厳しすぎる既存ディレクトリは、勝手に変更せず拒否します。

ポリシー変更と実行時警告には別々のイベント許可リストと `root:` / `runtime:` ID 名前空間を使用します。各取り込みでは、検証済みの root 所有ログからポリシー索引を先に再構築し、その中で ID を重複排除します。サービス所有の JSONL や SQLite の受領記録で root の取り込みを省略しません。不正な実行時レコードはスキップして件数を警告し、root の取り込みを妨げません。ダッシュボードも root ログを読み直し、サービス側の変更コピーを除外して `root` / `runtime` を表示します。同じ UID のプロセスによる実行時診断の偽造は引き続き可能です。

## エンジン制御ソケットの公開後に復旧する

同じホストの別のプログラム（開発者が起動したコンテナなど）が、root 所有のエンジン制御インターフェースを公開することがあります。`duduclaw` が Docker グループに属していなくても起こり得ます。警告と認証情報の保護状態には、パスと条件（書き込み可能、接続成功、予期しない errno）、または abstract socket 名と peer uid を表示します。直近の失効理由、詳細、UTC 時刻を `<home>/state/root-key-status.json` に保存します。`<home>` は通常 `/var/lib/duduclaw/.duduclaw` です。鍵そのものは記録しません。表示されたインターフェースを公開したプログラムを特定して停止してください。開発用コンテナは別のホストで動かすことを推奨します。古い containerd shim が検出された場合は containerd を更新してください。

自動復旧は既定で有効ですが、root 所有の再読み込み成功マーカーが unit と完全に同じ内容を持つことに加え、起動上限と終了コード 75 の再起動設定を含め、インストール済みの root 所有 unit が現在のテンプレートと完全に一致することを先に確認します。欠落、読み取り失敗、不一致の場合は自動再起動を無効にし、管理者による `service upgrade --protected-linux` が必要と表示します。アップグレードは内容が異なる unit とランチャーを原子的に置換し、`daemon-reload` を実行します。インストールとアップグレードでは unit 変更前にマーカーを無効にし、`daemon-reload` 成功後、root が `systemctl show` で drop-in を含む実効の起動間隔、起動回数上限、強制再起動の終了コードを検証した場合だけ原子的に書き込みます。検証に失敗するとマーカーは作成されず、自動復旧は無効のままです。既存のインストールもこの手順が必要です。自動復旧の対象は `engine_socket_reachable` と `engine_socket_probe_failed`（以前は検査失敗も前者のコードに含まれていました）です。どちらのポリシーでも不完全な検査は拒否または失効となり、その後の検査成功が復旧の条件です。失効後も同じ検査を 60 秒ごとに続け、3 回連続で成功すると通常の終了処理を 1 回要求します。途中で失敗すると回数をリセットします。gateway は終了処理の後に終了コード 75 で停止し、systemd が root ランチャーを再起動します。ランチャーと新しい gateway はすべての鍵受け取り検査を再実行し、成功した場合だけ鍵を受け取ります。拒否されたプロセス内で鍵を復元せず、鍵を保持しているプロセスでは復旧再起動を行いません。実行ファイルの所有者や権限、補助グループ、cgroup／unit、user namespace、コンテナ環境、鍵パスへのアクセスの問題は管理者による修復が必要で、自動復旧の対象外です。

時間あたりの上限は root 管理の systemd unit の `StartLimitIntervalSec=3600` と `StartLimitBurst=4` が強制します。systemd の計数区間で起動は合計 4 回までで、復旧、通常の障害、手動起動で枠を共有します。最初の起動も 1 回に数えるため、最初の区間では追加 3 回まで、次の区間では最大 4 回です。通常の障害用の別枠は設けません。`start-limit-hit` に達すると自動再起動は止まり、待つだけでは次の試行は予約されません。原因を修復した後、ホスト管理者が制限の許す時点でサービスを起動するか、明示的に失敗状態をリセットする必要があります。[systemd の起動制限](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html#StartLimitIntervalSec=interval)も参照してください。`<home>/state/root-key-recovery.json` の直近 1 時間の予算、ロック、診断履歴は補助情報です。同じ uid の社員は変更や書き込み妨害ができますが、systemd の計数はリセットできません。状態ファイルは固定したディレクトリ記述子から開き、シンボリックリンクを追わず、所有者、モード、単一ハードリンクを検査します。診断は既知の理由と復旧状態、有効な RFC3339 時刻、エスケープ後最大 512 bytes の詳細のみを受け付け、CLI、ログ、RPC では端末制御文字や不可視の書式文字を可視のエスケープ表記にします。状態ファイルを読み書きできない場合は自動復旧を止め、個人接続の停止を維持します。報告されたエラーからストレージとアクセス権を確認してください。再起動中は他の gateway 接続も短時間中断します。手動で再起動する従来の動作を使う場合は `config.toml` に次を設定します。

```toml
[credential_protection]
auto_recover = false
```

新しくインストールした unit には `Restart=on-failure` に加えて `RestartForceExitStatus=75`、そして `[Unit]` に上記 2 項目の起動制限が含まれます。`service upgrade --protected-linux` が書き込む unit とランチャーのテンプレートは、**更新を実行するバージョン**に組み込まれたものです。v1.79.3 からの最初の更新では旧プログラムが実行されるため、実行ファイルだけが置き換わり、旧 unit が残ります。旧 unit も `Restart=on-failure` で終了コード 75 を再起動できますが、時間あたりの制限がないため自動復旧は無効のままです。更新後に下記の `service install --protected-linux` を一度実行すると、新しい unit とランチャーが書き込まれ自動復旧が有効になります。以降の更新では、内容が異なる場合に自動で更新されます。ホスト管理者は systemd の計数をリセットできますが、home の JSON ではできません。新しいリリースを検証してインストールした後、ホスト管理者は `sudo /opt/duduclaw/bin/duduclaw service install --protected-linux` で組み込みのランチャーと unit を更新できます。データとルートキーを維持し、systemd を再読み込みし、稼働中の gateway を再起動します。ローカルの unit 上書き設定も確認してください。
