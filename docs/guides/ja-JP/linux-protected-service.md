# ネイティブ Linux の保護サービス

社員ごとの隔離構成（今後のバージョンで提供予定）では、gateway、AI 社員、文書解析、特権更新を分離します。確認済みの社員には永続的な Linux ID を割り当て、実行ごとにプロセス、ファイルシステム、ネットワークの namespace を作成します。必要な隔離コンポーネントが利用できなければ社員の実行を拒否します。

**開発状況：** 署名検証後の未有効化世代の配置、インストーラーから移行への接続、最初の管理者設定、読み取り専用の導入検査、固定パスの協調削除を実装しました。完全な新規導入、署名済み世代の有効化、移行プラットフォーム、gateway の総合準備検査は未完了です。今回 root 実機での導入・削除は検証していません。サービスの `active` や世代の配置だけでは隔離の準備完了を示しません。[実装と検証記録](../../architecture/native-linux-isolation.md)を参照してください。

**現在インストールできる内容：** `service install --protected-linux` は spawnd、egress、独立した更新サービスをインストールせず、移行計画を作成する処理もありません。隔離構成の新規インストール手順が完成するまで（今後のバージョンで提供予定）、保護サービスは下記「単一実行ファイルの保護サービス（現行版）」で説明する v1.80 の単一実行ファイル gateway サービスです。このガイドで説明する社員ごとの隔離は適用されません。

## 新規ホストへの隔離インストール（v1.83.0 以降）

v1.83.0 以降、新しいホストへの保護インストールは、独立して検証した `duduclaw-bootstrap install` でのみ行います。DuDuClaw を一度も導入していないホストで `sudo duduclaw service install --protected-linux` を実行すると、下記の手順を表示するだけで何も変更しません。`--migrate-from` はすべての保護インストールで拒否されます。v1.80〜v1.82 の単一実行ファイル保護サービスが動作している（またはその残りのファイルがある）ホストは、この手順では拒否されるため、アップグレード手順を使ってください。隔離インストールにはチーム版が必要です。

インストーラーはホストを変更する前に、次のストレージ要件を検査します。

- `e2fsprogs` をインストールします。`/usr/sbin/mkfs.ext4` の解決先は root 管理の通常の実行ファイルで、親ディレクトリも信頼できる必要があります。
- カーネルの loop モジュールを利用可能にします（`modprobe loop`）。`/dev/loop-control` はキャラクターデバイス 10:237 である必要があります。
- `/var/lib` のあるファイルシステムには、20 GiB または総容量の 10%（大きい方、端数切り上げ）以上の空き容量が必要です。既存の「パッケージサイズの 2 倍 + 1 GiB」の検査も満たす必要があります。下位に device-mapper がある場合は `dmsetup`（`dmsetup`／`lvm2` パッケージ）も必要です。

1. root でダウンロード用ディレクトリを作成します：`install -d -m 0700 -o root -g root /root/duduclaw-migrate`。
2. 次のファイルをファイルとしてこのディレクトリに保存します：`duduclaw-bootstrap`、`duduclaw-isolated-linux-<アーキテクチャ>-<バージョン>.tar` とその `.minisig`、`.authorization.json`、`.authorization.json.signatures.json`、およびチーム版ライセンス `license.json`（root 所有で、グループとその他のユーザーが書き込めないもの）。
3. 公式サイトとライセンスコンソールで公開されている値と `sha256sum -c` で照合します。両方が一致している必要があります。
4. `sudo /root/duduclaw-migrate/duduclaw-bootstrap install [--dns-upstream <IP:53>] [--host-public-address <IP>]...` を実行します。インストーラーが gateway を自分で起動し、起動時の自動起動も設定します。

| オプション | 意味 |
| --- | --- |
| `--dns-upstream <IP:53>` | systemd-resolved のないホストでのみ使用：専用 DNS 転送サービスの唯一の上流。systemd-resolved がある場合は拒否されます。ループバックアドレス（`127.0.0.1` 上のローカル DNS など）と IPv4 リンクローカルアドレス（`169.254.0.0/16`。クラウド VM のメタデータサービスや DNS のアドレスなど）も拒否されます。DNS 転送サービスのネットワークルールはホストを限定するだけでポートは限定しないため、転送サービスからメタデータサービスにも届いてしまうからです。唯一の DNS リゾルバーがリンクローカルアドレスのクラウド VM では、systemd-resolved をインストールして有効にするか（インストーラーがそれを直接使い、`--dns-upstream` は不要になります）、ルーティング可能なアドレスのリゾルバーを指定してください。インストーラーはこのアドレスを DNS 転送サービスのネットワークルールに書き込みます。社員の外部接続ルールには追加されません。上流を変更するにはインストーラーを再実行してください。DNS 転送サービスのユニットファイルを編集しないでください（準備状態チェックは編集されたファイルを拒否します）。ドロップインも追加しないでください（隔離実行サービスのネットワークルール確認が拒否します）。 |
| `--host-public-address <IP>` | ネットワークインターフェースにないこのホストの公開アドレス（クラウドの 1:1 NAT）。複数指定可、最大 64 個。社員はこのアドレスに接続できません。 |
| `--enable-now` | 不要になり、効果もありません：インストーラーは常に gateway を実行中のままにします。既存のスクリプトがそのまま動くよう引き続き受け付け、指定された場合は 1 行の案内を表示します。 |

インストーラーは何かを変更する前にホストを検査し、不合格の項目をまとめて表示します：Ubuntu 22.04、Ubuntu 24.04 または Debian 12、systemd 249 以上、カーネル 5.15 以上、cgroup v2、`/var/lib` が ext4 または XFS、`/etc/login.defs` のシステム ID 上限が有効であること（標準ファイルでは `SYS_UID_MAX`／`SYS_GID_MAX` はコメントアウトされています。未設定の場合は shadow と同じく `UID_MIN`／`GID_MIN` から 1 を引き、下限の既定値は 1000 です）、70000〜99999 にアカウント・グループ・subordinate ID の範囲がないこと、sshd のメイン設定が `/etc/ssh/sshd_config.d/*.conf` を読み込むこと、`cron.allow`／`at.allow` が DuDuClaw のアカウントを許可していないこと、十分な空き容量、有効なチーム版ライセンス、以前の DuDuClaw のインストールがないこと。外部アカウントサービス（LDAP、SSSD など）を使うホストは、このインストーラーではサポートしていません。

続いて、サービスアカウントのプロセスがないことを確認し、検証済みの更新信頼ルート・世代・固定パスのプログラムを配置し、サービスアカウント・ディレクトリ・サービスユニットを作成し、署名済みポリシーから外部接続と DNS の設定を書き込みます（gateway のユニットもこの段階で配置しますが、まだ有効化しません）。次に、起動時と同じ順序でサービスを起動します：各ソケットと spawnd、整合性ゲート（spawnd が開いた状態）、egress、gateway、最後に更新サービス。更新サービスがインストールしたバージョンを報告するまで（最大 2 分）、spawnd がすべての準備検査の合格を報告するまで（最大 2 分 30 秒）待ちます。spawnd が時間内に準備できない場合、メッセージにまだ合格していない検査が表示されます。その後、夜間更新タイマー（`duduclaw-auto-upgrade.timer`）を有効にし、総合準備検査を実行して、合格した後にだけ gateway の自動起動を有効化します。gateway の実行中に完全な準備検査に合格した場合にのみ、インストールは成功を報告します。

**再開：** 進捗は `/var/lib/duduclaw-protected/isolation-bootstrap.json` に記録されます。インストールが中断した場合（エラー、再起動、停電）は、表示された原因を直してから同じ `duduclaw-bootstrap install` コマンドをもう一度実行すると、最初の未完了の手順から続行します。再開時はオプションを省略できます。異なる値は拒否されます。別のインストールパッケージはロールバックするまで拒否されます。インストールが未完了の間、`service install`、`upgrade`、`uninstall --protected-linux` は拒否されます。

**ロールバック：** 最後の手順（確定）の前であれば、`sudo /root/duduclaw-migrate/duduclaw-bootstrap rollback`（ダウンロード用ディレクトリに同じパッケージが残っている必要があります）でホストをインストール前の状態に戻せます。サービスを停止・無効化した後、導入したプログラムとサービスユニット、更新サービスの信頼ルート・状態・世代、spawnd の状態、`egress.json`、`dns-broker.json`、ルート鍵、今回のインストールで作成された gateway のデータを削除します。システムアカウント、空の gateway データディレクトリとライセンス、保護モードのマーカー、古いパッケージのインストールを防ぐ記録は残します。削除はリンクをたどらず、マウントポイントをまたぎません。何か残った場合はそのパスを表示し、同じコマンドを再実行できます。確定後はロールバックできません。`service uninstall --protected-linux` を使ってください。

## 利用者にとっての変更

以下の変更は隔離構成が提供されてから適用されます。現在のインストールには適用されません。

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

`getent`、`id`、`useradd`、util-linux、systemd ツール（`systemd-tmpfiles` を含む）、curl などの信頼できるシステムツールが必要です。社員の loop ストレージには ext4 または XFS、対応する非 thin provisioning の下位デバイス構成、社員のクォータと TCB の予約容量が必要です。不明な構成や容量不足は準備完了を拒否します。`/etc/nsswitch.conf` の passwd 行と group 行に書けるのは `files` と `systemd` だけです（Ubuntu・Debian の既定値、および authselect の `minimal` プロファイルを使う RHEL／Fedora）。それ以外のモジュール（LDAP／SSSD の `sss`、Google Compute Engine OS Login の `oslogin`／`cache_oslogin`、`compat`、`[SUCCESS=merge]` のような角括弧のアクション句）を含むホストはサポート対象外で、準備完了を拒否します。こうしたモジュールは社員 UID 範囲にアカウントを持ち込む可能性があり、このリリースでは外部ディレクトリ側でその範囲を予約できないためです。

## 単一実行ファイルの保護サービス（現行版）

隔離コンポーネントをインストールできるようになるまで（今後のバージョンで提供予定）、`service install --protected-linux` は v1.80 の単一実行ファイル保護 gateway をインストールします。root 所有のプログラム `/opt/duduclaw/bin/duduclaw`、ランチャー `/usr/local/libexec/duduclaw/protected-launcher`、ユニット `duduclaw-gateway.service`、バージョン下限 `/var/lib/duduclaw-protected/version-floor` で構成されます。ホストに隔離構成がインストールされるまで（今後のバージョンで提供予定）、AI 社員は v1.80 と同じく gateway のサービスアカウントで実行され、v1.80 の認証情報保護が適用されます。社員も v1.80 と同じくダッシュボードから直接作成できます。このガイドで説明する隔離実行（spawnd による起動、個人専用社員、社員配置の提案）は、隔離構成が実際にインストールされたホストでのみ有効になります。

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
| セルフテスト用アカウント | `duduclaw-selftest`：隔離セルフテストが権限を下げて使うアカウント。他のサービスアカウントと同じシステム ID、ホームなし、ログイン不可、専用グループ、追加グループなし、ロック済み |
| セルフテスト用ユニット | `/etc/systemd/system/duduclaw-isolation-selftest.service`。root:root 0644、同梱版と完全に同一でドロップインなし。`[Install]` がなく、インストーラーは有効化も起動もせず、spawnd が起動する |
| SSH 拒否検査プログラム | `/usr/lib/duduclaw/duduclaw-isolation-probe`。root:root 0755、署名済みコンポーネント。特権のない隔離テスト用アカウント `duduclaw-selftest` で journal の読み取り権限（`systemd-journal` グループ）付きで実行し、root では決して実行しない。専用アカウントはない |
| SSH 拒否検査ユニット | `/etc/systemd/system/duduclaw-isolation-probe@.service` テンプレート。root:root 0644、同梱版と完全に同一。`[Install]` がなく、インストーラーは有効化も起動もしない。社員アカウントと sshd ポートごとに 1 インスタンス（`duduclaw-isolation-probe@duduclaw-emp-<uid>.<port>.service`） |
| 解析用 root | `/usr/lib/duduclaw/parse-root` と `/usr/lib/duduclaw/parse-root-tree`。root:root 0755、ホストデータを置かない。`parse-root` は初期状態で空。`parse-root-tree` には検証済みのパーサー、その実行時ライブラリ、パーサーユニットが必要とする空のマウントポイントディレクトリ（`.tmp`、`dev`、`proc`、`run`、`sys`、`tmp`、`var`、`var/tmp`）だけがあり、すべてインストーラーが作成する |
| 署名済み状態 | Bootstrap の信頼、互換性のある世代、runtime 登録、更新スロット／状態、移行／準備完了の連携 |

解析器は[インストール参考](../../../crates/duduclaw-parse-worker/README.md)に記載された既知の空マウントポイントを残す場合があります。予期しない内容は調査し、解析用 root を再帰的に削除しないでください。Spawnd が社員の SSH、cron、at、user manager を封鎖し、ホストの実効状態を検証します。検査に通すために制御を緩和しないでください。

`/usr/sbin/sshd` がインストールされているホストでは、各社員アカウントの SSH が拒否されることを spawnd 自身が証明します。インストーラーの受領記録（レシート）は使いません。spawnd はまずそのアカウントに実際に適用される sshd 設定を自分で確認し、次にそのアカウントと sshd ポート用の SSH 拒否検査ユニットを systemd 経由で起動します。検査プログラムは特権のない隔離テスト用アカウント `duduclaw-selftest`（journal の読み取り権限付き）で動作し、その社員アカウントで稼働中の sshd に対して実際の公開鍵ログインとポート転送を 1 回ずつ試みます。どちらも `DenyUsers duduclaw-emp-*` ルールで拒否され、sshd 自身の journal 記録で確認できる必要があります。アカウントがログインできると判定するのは、検査プログラムの鍵を受け入れたことを sshd 自身が記録した場合だけです。sshd が記録していない成功（たとえば SSH ポートで別のプログラムが応答した場合）は判定不能として扱います。この証明はメモリ上にのみ保持され、sshd の再起動、そのアカウントに実際に適用される sshd 設定、アカウント、インストール済みの検査プログラムやそのユニット、署名済み世代、起動のいずれかが変わったとき、spawnd の再起動のたび、そしていずれにせよ少なくとも 6 時間ごと（spawnd が systemd 経由で稼働中の sshd を識別できない場合は 10 分ごと）に証明し直されます。更新は古い証明の期限が切れる前に始まるため、正常なホストでは空白期間が生じません。ある社員の証明が届くまでは、その社員だけが実行できない状態（not ready）になり、他の社員には影響しません。spawnd は新しい社員の作成で証明を待ちません。最初の証明が進行している間、spawnd は「保留中」と応答し、gateway は何も取り消さずに同じ割り当て要求を最大 2 分間再送します。それでも証明が終わらない場合、項目は「適用中」のまま残り、移行の確認にアカウントのセキュリティ検証が未完了と表示されます。spawnd が動いている間かつ 10 分以内にその項目を再試行すると同じアカウントで再開します（それを過ぎるか spawnd を再起動すると未完了のアカウントは廃止され、適用は廃止済みアカウントと同様に失敗し、作成を提案し直す必要があります）。保留中にできる新しい社員は同時に 4 人までで、そのうち 1 人の結果が出るまでそれ以上の作成は拒否されます。証明でそのアカウントがログインできると判明した場合、そのアカウントは恒久的に廃止されます。

sshd のあるホストには `/usr/bin/ssh`、`/usr/bin/ssh-keygen`、`/usr/bin/journalctl`、`duduclaw-selftest` アカウント（インストーラーが作成）と `systemd-journal` グループも必要で、sshd は 127.0.0.1（設定の最初の `Port`）で待ち受け、ログを systemd journal に出力している必要があります。`UsePAM no` の sshd はサポート対象外です。その場合 sshd は `DenyUsers` を適用する前に期限切れの社員アカウントとして拒否するため、検査は拒否を確認できず、その社員は準備完了になりません。sshd のないホストではこの検査を省略します。

インストーラーは固定パスの完全なコンポーネント群を検証してから、アカウント、ディレクトリ、unit を準備します。Linux 配布物には固定更新ランチャーも含まれ、オフラインツールは署名済み情報とファイルから bootstrap inbox を作成できます。独立に認証された `duduclaw-updated --bootstrap-stage` は、コンパイル時の root 指紋、root 閾値署名、target 署名、全ファイルのハッシュを検証し、**未有効化の世代**を配置します。`current` の選択、trust／state／slots の作成、旧版移行の二重署名検証、サービス有効化は行いません。完全な新規導入の手順はまだ提供していません。

`sudo duduclaw service readiness --protected-linux`（`ops` は同じ入口の別名）は、実行ファイル、システムアカウント、ディレクトリ権限、unit 本文、systemd の実際の属性を読み取り専用で検査し、個別に報告します。失敗した各グループの下には、その理由も表示されます。I1 の総合検査アダプターは未接続のため、`MissingComposite` が必ず拒否し、社員実行を許可しません。カスタム DNS drop-in は未対応で unit 検査を拒否します。すでに新方式で管理される世代の再インストールは、この読み取り専用検査のみを行い、実行ファイルや unit を書き換えません。

このコマンドは 2 つの結果を分けて報告します。**プラットフォーム準備状況**（インストーラーの 4 グループとホストの 9 項目）だけを検査するのが `sudo duduclaw service readiness --protected-linux --platform-only` で、すべて合格すると最後に「平台檢查均已通過。」（プラットフォーム検査に合格）と表示し、社員の実行が可能とは主張しません。`--platform-only` を付けない場合は、Gateway 自身の準備状況も読み取ります。稼働中の Gateway は、内容が変わったとき、および少なくとも 30 秒ごとに `/run/duduclaw-gateway-update/isolation-readiness.json` を書き換えます（固定のコンポーネント名・理由名・起動要素名のみ）。Gateway の状態がプラットフォーム検査の結果を変えることはありません。プラットフォーム検査がすべて合格した後の結果は 3 通りです。

| Gateway の状態 | 最後の行 | 終了コード |
|---|---|---|
| 今回の起動のもので 90 秒以内、準備完了 | 「所有就緒檢查均已通過，可以執行員工工作。」（またはデータ移行の確認待ちの文） | 0 |
| 新しい状態だが未準備 | `[資訊]` 行に未準備のコンポーネントを `名前(理由)` で、起動組み立てに不足している要素とともに列挙し、続けて「平台檢查均已通過，但 Gateway 尚未就緒，員工工作仍暫停。」 | 0 以外 |
| 存在しない、古い、別の起動のもの、または無効（Gateway が停止中か起動直後） | 「[資訊] 無法取得 Gateway 的就緒狀態（Gateway 未執行、剛啟動，或狀態檔無效）。」、続けて社員の実行を確認できないという拒否 | 0 以外 |

インストーラー自身の準備状況ステップはプラットフォームの結果のみを使います。インストール直後は、Gateway の隔離ツールサービスがまだバックグラウンドで組み立て中の場合があるためです。Gateway のジャーナルにも同じ不足要素が表示され、理由が分かる場合は固定の理由が付きます。例：`ConnectorProviders(catalog_file_unsafe)`（コネクターカタログ `isolation-connectors.db` またはその SQLite 補助ファイルが、Gateway アカウント所有の所有者専用・単一リンクのファイルではない）、`ExecutionPlans(invalid_helper_runtime)`（署名済みの補助ランタイムが固定プロトコルと一致しない。補助ランタイムがまったくないポリシーは有効で、その場合それらのツール群は提供されないだけです）。Gateway はバックグラウンドで組み立てを再試行するため、原因を直した後に再起動は不要です。

## v1.79.3 からの移行

旧バイナリをその場で交換したり、旧データベースを新しい home にコピーしたりしないでください。旧環境は社員と UID を共有するため、旧権限データは信頼できません。インストーラーは v1.79.3 から v1.80 への境界（旧バージョン下限が不明で、新しいホストとも確認できない場合を含む。上記の単一実行ファイルの節を参照）を G の移行処理へ渡し、root 所有の固定計画 `/var/lib/duduclaw-migrate/plan.json` を読みます。計画が欠落・不正なら旧サービス停止前に拒否し、旧方式の上書き更新には戻りません。

管理者の手順は次のとおりです。

1. 公式サイトと認証済みライセンス console の両方から、bootstrap 検証プログラムと移行パッケージの SHA-256、および root metadata の指紋を取得し、一致を確認します。root 所有の 0700 ディレクトリへファイルとして保存し、検証プログラムの実行前にシステムの `sha256sum -c` で照合します。旧アーカイブの署名だけでは新しい信頼を確立できません。ダウンロードを shell へパイプしないでください。
2. 検証済みインストーラーが bootstrap 署名とプラットフォームを確認し、root 所有の不変な移行計画を作成します。計画を編集して権限や成功証跡を作らないでください。
3. 移行プログラムは**旧サービスを停止する前に**プラットフォームを検証します。その後、旧起動経路の無効化、旧データの隔離、検証済み暗号化スナップショット、新しい鍵、空の取り込み専用 gateway を準備します。低権限インポーターは既知の内容を制限付きレコードへ変換し、権限データは無効な提案として保存します。
4. 隔離の検証後、インストーラーの設定手順で新しい管理者を作成します。旧 session、パスワード、MFA、CLI ログイン状態は復元されません。高リスク項目の確認前に MFA を登録します。
5. **システム → 移行の確認**で完全な値を確認し、承認／拒否、適用失敗の再試行、認証情報の交換を行います。社員 ID とストレージは確認後に割り当てます。SSH のあるホストでは、各社員の最初の割り当てにアカウントの SSH 拒否の検証で最大 1 分ほどかかります。検証が未完了と表示された場合は、その項目を再試行してください。個人アカウントは新しい所有者として再接続します。
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

## 隔離社員のモデル接続情報

OpenAI 互換 runtime を使う隔離社員は、自分のホームにある `/home/employee/.duduclaw-api/providers.json` だけからエンドポイントと API キーを読みます。gateway のアカウント、設定、環境変数は使われません。ホスト管理者は root として次のコマンドでこの情報を書き込み、または削除します。

```sh
sudo duduclaw service provider set --protected-linux --employee <社員名> < providers.json
sudo duduclaw service provider set --protected-linux --employee-id <小文字 16 進数 32 文字> < providers.json
sudo duduclaw service provider set --protected-linux --employee <社員名> --clear
```

文書は**標準入力からのみ**読みます。コマンドライン引数、環境変数、ファイルパスでは受け付けません。ファイルをリダイレクトせずに端末で実行すると、形式のヒントを表示して拒否します。上限は 65,536 バイトです。形式:

```json
{"providers":[{"name":"main","api_key":"<キー>","base_url":"https://provider.example/v1"}]}
```

- 名前が重複しない 1〜32 個のプロバイダーを指定できます。`base_url` は https で、ユーザー情報、クエリ、フラグメントを含めないでください。`protocol`（`chat_completions`、`responses`、`anthropic`、`gemini`）は任意です。形式が正しくない場合は固定のメッセージだけを表示し、内容は表示しません。
- キーは**その社員自身のホーム**（社員が所有、権限 0600）に保存され、その社員の runtime が読めます。この社員専用のキーを使い、gateway や共有アカウントのキーは入れないでください。
- このコマンドでは、変更のたびにホストの root が必要です。ホストで実行中の作業がなく、その社員が使用中でないときだけ書き込みます。それ以外の場合は後で再試行してください。実行するたびに書き直します。成功時は社員、プロバイダー数、各プロバイダーの名前と `https://ホスト[:ポート]` だけを表示し、キーは表示しません。
- 削除するには `--clear` を付けます（標準入力は読みません）。社員を無効化した場合も削除され、社員を退役させた場合はストレージとともに破棄されます。

### ダッシュボードからの設定

gateway は同じ文書をダッシュボードからも受け付けます（gateway 側は実装済みで、ダッシュボードの画面は後続のパッケージで提供します）。

- 設定・削除できるのは現在の Admin だけで、事前に本人確認（step-up：パスワードと 2 要素認証、5 分間有効、そのダッシュボードのセッションに紐づく。接続アカウントや移行の判断と同じ確認）が必要です。割り当て済みの隔離社員であれば、他の人の個人専用社員も対象にできます（接続アカウントと同じ規則）。すべての操作は、操作者、社員、結果、各プロバイダーの名前とエンドポイントのオリジンとともに監査ログに記録されます。キーやエンドポイントのパスは記録しません。
- gateway はキーをメモリ上で root に渡すだけで、root がホストコマンドと同じ方法で社員のホームに書き込みます。キーは gateway のディスク、データベース、設定、ログ、監査、エラーメッセージに書かれず、受け渡し後に gateway のメモリ上のコピーは消去されます。検証規則はホストコマンドと同じです。加えて、名前、キー、エンドポイントに引用符、バックスラッシュ、制御文字が含まれる場合は拒否します。
- ダッシュボードの状態表示は、社員ごとにプロバイダー名、プロトコル、エンドポイントのオリジン（`https://ホスト[:ポート]`）、ダッシュボードで最後に確認された変更の時刻だけを示します。**ダッシュボードで行った変更だけを反映します**。上記のホストコマンドで書き込み・削除した文書はここには表示されません。他の人の個人専用社員は、Admin が本人確認して表示するまで隠されます（監査ログに記録）。
- ホストで実行中の作業がある、またはその社員が使用中の場合は「使用中」として拒否します。社員が空いてから再試行してください。隔離サービスが変更を確認できなかった場合は「未確認」と表示し、状態は変わりません。同じ変更を再送しても安全です。

### 社員が実行できないとき

隔離実行が失敗すると、gateway のジャーナルに警告 `isolated run refused` が 1 件記録されます。フィールドは `runtime`、`agent`（社員ラベル）、`reason`、`class`、`exit`（終了コード、または `signal`、`timeout`、`incomplete`）、`stderr_len` です。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep 'isolated run refused'
```

OpenAI 互換ランタイムの `reason` はワーカーの固定理由です（一覧は `crates/duduclaw-api-worker/README.md` の「Diagnostics」）。他のランタイム、および既知の理由 1 つと完全に一致しない出力は `reason="unrecognised"` になります。ランタイム自身の出力はログにも画面にも出しません。チャットチャネルでは、管理用の診断を見られる人には次の固定メッセージ（繁体字中国語）のいずれかが表示されます。それ以外の人には一般的な失敗メッセージ、Web サイトの公開訪問者には訪問者向けメッセージだけが表示されます。

| 表示されるメッセージ | `class` | 確認すること |
| --- | --- | --- |
| 這位 AI 員工尚未設定模型連線資料，或設定內容無法使用；請管理員到『受保護模式 → 模型連線資料』檢查。 | `employee_credentials` | この社員のモデル接続情報があり、形式が正しく、要求されたプロバイダーを含むこと（上記参照）。 |
| 連不到這位 AI 員工設定的模型服務；請管理員檢查端點位址與對外連線。 | `provider_transport` | `base_url` のホスト、TLS、外部通信ポリシーがそのホストを許可していること。 |
| 模型服務拒絕了這次請求；請管理員檢查這位 AI 員工的金鑰與模型名稱。 | `provider_refused` | API キー、モデル名、プロバイダー側の利用枠。 |
| 模型服務的回覆無法使用；請管理員檢查端點是否為相容的服務。 | `provider_response` | `base_url` と `protocol`（`chat_completions`、`responses`、`anthropic`、`gemini`）がプロバイダーの API に合っていること。 |
| 這次工作超過時間上限而停止。 | `timeout` | この作業の制限時間、またはプロバイダーの応答の遅さ。 |
| 受保護模式尚無這次工作的隔離執行授權，或所選執行設定尚未通過驗證；請由管理員檢查服務狀態。 | その他、または `unrecognised` | 準備完了の状態とジャーナルの `reason`、`exit`。「準備完了エラーの調査」を参照。 |

作業が**実行の開始前に**拒否された場合は、代わりに警告 `isolated run not started` が 1 件記録されます。フィールドは `runtime`、`agent`（社員ラベル）、`site` です。`site` は拒否された場所を示す固定ラベルです。実行が始まったのに上の行を残さずに終わった場合（パイプがない、または正常終了したが結果を使えない）は、同じフィールドで `isolated run refused after start` が記録されます。失敗した保護作業 1 件につき、この 3 種類のうち 1 行だけが記録されます。リクエストの内容、パス、社員ラベル以外の識別子は記録しません。利用者に表示されるメッセージは変わりません。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated run (not started|refused)'
```

| `site` のグループ | ラベル | 確認すること |
| --- | --- | --- |
| 事前チェック（権限はまだ使われていない） | `preflight.model`、`preflight.runtime_policy`、`preflight.model_policy`、`preflight.redaction`、`preflight.interceptor`、`authorize.agent_dir_missing`、`authorize.effort`、`authorize.utility_call`、`authorize.work_dir_override`、`authorize.agent_dir`、`authorize.capabilities_policy`、`authorize.native_gate`、`authorize.runtime_kind`、`authorize.explore_lane`、`authorize.tool_restrictions`、`authorize.codex_home`、`authorize.product_policy`、`authorize.interceptor`、`authorize.runtime_id`、`authorize.max_turns`、`step_limit.agent_dir`、`step_limit.unavailable` | 社員の `agent.toml`（`[model] effort`、`[capabilities]`）、ランタイムとモデルの製品ポリシー、マスキングと CCR の設定。 |
| 認可（1 回限りの実行権限） | `authorization.ingress.*`（依頼した人：`no_scope`、`consumed`、`capture.*`（メッセージ受信時のサインイン確認：`no_web_credential`、`web_identity`、`channel_conflict`、`channel_mapping`、`requester`、`user_db`、`user_inactive`、`epoch`、`blocking`）、`agent_path`、`home_mismatch`、`no_turn`、`no_session`、`no_attended_proof`、`attended_proof_invalid`、`nested_dispatch`、`principal_mismatch`、`proof_expired`、`identity_recheck`、`blocking`）、`authorization.issue.*`（権限の発行：`boot_owner`、`product_authority`、`lifecycle`、`snapshot`、`epoch`、`principal`、`binding`、`policy_revision`、`tools`、`human`、`selectors`、`capability`、`users_recheck`、`policy_changed`、`lifecycle_recheck`、`snapshot_recheck`、`facts_recheck`）、および `authorization.consumed`、`authorization.installed_mismatch`、`authorization.agent_label`、`authorization.home`、`authorization.principal_context`、`authorization.background_issue`、`authorization.ingress_issue` | その人のアカウントが有効で、パスワード変更が不要で、この社員に作業を割り当てられること。サインインが有効なこと。社員の所属（共有の組織または個人の所有者）と割り当ての状態。 |
| 実行計画 | `request.encode`、`plan.readiness`、`plan.boot_owner`、`plan.home_mismatch`、`plan.employee_unconfirmed`、`plan.root_publication`、`plan.blocking`、`plan.unavailable` | 準備完了の状態。社員が確認済みであること。root の公開設定がこの社員にこのランタイムを提供していること。 |
| 起動準備 | `launch.readiness`、`launch.boot_owner`、`launch.boundary_unhealthy`、`launch.employee_unconfirmed`、`launch.employee_mismatch`、`launch.product_prepare`、`launch.binding`、`launch.blocking`、`launch.resolver`、`launch.connectors`、`launch.worker_plans`、`launch.factories`、`launch.dispatcher`、`launch.protected_launch`、`launch.authority_pairing`、`launch.bind.<step>`（実行の予約後。`<step>` は下の `isolated launch preparation refused` と同じ） | この社員の保護されたツール境界（ツールの投影、コネクター）。`launch.bind.*` は下の表を参照。 |
| 実行開始の依頼 | `spawn.prepare`、`spawn.not_protected`、`spawn.admission`、`spawn.backend.unavailable`、`.denied`、`.would_deadlock`、`.not_found`、`.timeout`、`.io`、`.protocol`、`spawn.start.permission_denied`、`.not_found`、`.timed_out`、`.other` | `spawn.backend.*` は隔離サービス（またはその制御ソケット）が拒否したときの固定分類です。隔離サービスは開始の拒否をログに残さないため、`duduclaw-spawnd` のジャーナルに何もなくても、このグループは除外できません。 |
| 開始後（`isolated run refused after start`） | `run.stdin_missing`、`run.stdout_missing`、`run.stderr_missing`、`run.input`、`run.output_drain`、`run.stderr_drain`、`run.wait`、`run.refused`、`output.parse`、`output.empty`、`output.utf8` | ランタイムの結果。`isolated run refused` の行がすでにある場合は追加で記録しません。 |
| `unlabelled` | — | ラベルのない拒否。不具合なので報告してください。 |

`spawn.backend.*` だけでは、開始を拒否したのがゲートウェイか隔離サービスかは分かりません。ゲートウェイ自身が拒否した場合（開始の依頼を送る前、または送らずに）、`isolated run not started` の行の直前に警告 `isolated spawn refused locally` が 1 件記録されます。フィールドは固定の `step` だけです（識別子、パス、内容は含みません）。`spawn.backend.*` の行の前にこの警告がない場合は隔離サービスが拒否しています（隔離サービスはこれをログに残しません。上の表の「実行開始の依頼」の行を参照）。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (spawn refused locally|run not started)'
```

| 時点 | `step` | 意味 |
| --- | --- | --- |
| 実行の予約 | `admission`、`tracked_runs`、`reservation_reply`、`reservation_queue`、`endpoint_directory`、`endpoint_socket`、`endpoint_connect`、`peer_identity` | 準備状態が変わった。ゲートウェイの実行表が満杯。予約の応答が使えない（有効期限はゲートウェイの時計で 30 秒以内である必要があります）。この社員はこのゲートウェイで未終了の実行を既に 32 件持っている。制御ソケットのディレクトリ、ソケット、接続先の確認に失敗した。 |
| 予約後、開始前 | `launch_prepare`、`policy_binding`、`task_binding`、`pty_size` | 保護された起動コンテキストが拒否した（ゲートウェイによるこの実行の権限の再確認、またはツール境界による実行用権限の発行。準備状態、社員の所属、ランタイムの MCP 登録を確認）。準備した計画が予約した実行と一致しない。予約の終了後にタスクを結び付けた。端末サイズが無効。 |
| 開始の依頼の作成 | `admission`、`workspace_delivery`、`workspace`、`entry`、`reservation_state`、`resource_limits`、`inputs`、`attachments`、`request_invalid`、`connection_state`、`live_check`、`fd_plan` | 準備状態が変わった。ワークスペースの証明が一致しない。計画が社員自身以外のワークスペースを指定している。実行が不明、別の社員のもの、開始済み、または 30 秒の予約が期限切れ。ランタイムの記録で出力または入力の上限が 0。入力が上限を超えた。添付が多すぎる。依頼が無効（例：ステップ上限が 1〜1000 の範囲外）。制御接続が閉じている。権限の即時確認が拒否した。記述子の構成が一致しない。 |
| 送信後 | `admission_after_send`、`start_reply` | 開始の依頼の送信中に準備状態が変わった（実行は取り消されます）。想定外の応答。 |
| 開始後、ツールの提供前 | `take_io`、`take_mcp`、`launch_activate` | 開始済みの実行が待機・起動時間内に標準ストリーム（または端末）を渡さなかった、またはその受け渡しから MCP エンドポイントを取り出せなかった（このときエンドポイントは有効化されません）；保護された起動コンテキストがこの実行の MCP エンドポイントを拒否した（下の `isolated launch activation refused` を参照）。開始済みの実行は取り消され、実行用権限も無効化されます。 |
| その他の制御依頼 | `cancel_state`、`live_status_reservation`、`work_op`、`helper_op` | 取り消しが中断済み、または既に拒否された。状態の応答が別の予約を指している。その経路では許可されていない操作。 |

`step` が `launch_prepare` のとき、ゲートウェイはその直前にもう 1 件、警告 `isolated launch preparation refused` を記録します。フィールドは拒否した確認を示す固定の `step` だけです（識別子、パス、内容は含みません）。そのターンの `isolated run not started` の行には `spawn.backend.denied` ではなく `site="launch.bind.<step>"` が表示されます。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (launch (preparation|activation) refused|spawn refused locally|run not started)'
```

| `step` | 意味 |
| --- | --- |
| `recheck.expired` | 実行権限の期限切れ。権限は保護された実行を依頼した時点で発行され、有効期間は 30 分で、Web サインイン自体の有効期限を超えません。サインインし直してください。システム時計のずれもここに表れます。 |
| `recheck.agent` | 社員がゲートウェイの現在の社員一覧にもういない。 |
| `recheck.identity` | 権限の発行後に、社員のライフサイクル（割り当て、確認、root の読み戻し）またはその人のアカウント情報（組織、アクセス世代、ポリシー世代、サインイン世代）が変わった。例：適用中の社員承認や所属の変更、全端末からのサインアウト、二要素認証の変更。 |
| `recheck.policy_revision` | ポリシーの入力ファイルが存在するのに読めない・解析できない、社員の `agent.toml` に `[agent] status = "active"` がない、または `config.toml` がない。入力：社員の有効な `agent.toml`、`CONTRACT.toml`（省略可）、`preset_bindings.toml`、`org.toml`、`config.toml`、カスタムのマスキング設定、`KILLSWITCH.toml`。 |
| `recheck.revision_changed` | 権限の発行後にポリシーの入力が変わった：`agent.toml` の `[capabilities]`、`[permissions]`、`[agent]` の `reports_to`／`department`／`role`、`CONTRACT.toml`、プリセットの割り当て、`org.toml` の上位の連鎖、`config.toml` の `[delegation]`、`[acp]`、`[provenance]`、`[integrations]`、`[redaction]`、カスタムのマスキング設定、`KILLSWITCH.toml`。会話履歴、記憶、`.claude/`、`.mcp.json`、`SOUL.md`、スキルは入力ではありません。 |
| `recheck.epoch`、`epoch_after_snapshot` | 確認の途中（下の状態問い合わせの前または後）にその人のアカウント情報が変わった。 |
| `recheck.background` | スケジュールや委任の作業のみ：そのバックグラウンド記録がもうこの実行を認可していない。 |
| `live_snapshot` | 予約した実行について隔離サービスの状態応答が失敗した、または 250 ミリ秒以内に届かなかった（その後この実行の制御接続は閉じられます）。`duduclaw-spawnd` のジャーナルとホストの負荷を確認してください。 |
| `binding_mismatch` | 隔離サービスが、この実行を別の社員のもの、またはもう予約状態ではないと報告した。 |
| `revoked_before`、`revoked_after` | 準備中に実行が取り消された（例：ターンが停止された）。 |
| `verify_blocking` | 権限確認用のブロッキング処理プールが 2 秒間空かなかった、または確認に 10 秒以上かかった（`users.db`／`apply.db` や root の読み戻しが遅い）。 |
| `prepared_missing`、`state_lock`、`associations` | 同じ起動が 2 回準備された、状態が壊れた、またはゲートウェイの結び付け済み実行の表（4096 件）が満杯か既にこの実行を含んでいる。不具合なので報告してください。 |

ゲートウェイ自身の確認を通過すると、ツール境界がこの実行を登録します（`inner_prepare.<substep>`）。各ステップ：

| `step` | 確認すること |
| --- | --- |
| `inner_prepare.revoked`、`inner_prepare.revoked_after` | ツール境界の準備中に実行が取り消された。 |
| `inner_prepare.reservation.<class>`（`unavailable`、`denied`、`would_deadlock`、`not_found`、`timeout`、`io`、`protocol`） | ゲートウェイが持つ予約の記録：`denied` は 30 秒の予約の期限切れ、制御接続が閉じた、または開始済み。`not_found` はこのゲートウェイが実行を知らない。 |
| `inner_prepare.plan_class`、`inner_prepare.plan_runtime`、`inner_prepare.plan_pty` | このターン用に準備した計画の実行クラス、ランタイム、端末モードが実際の起動と一致しない。不具合なので報告してください。 |
| `inner_prepare.registry.publication_read`、`inner_prepare.registry.generation` | ゲートウェイが root のランタイム公開情報 `/run/duduclaw-spawnd/publication.json` を読めない（所有者、グループ、権限、別の起動、または 1 秒の有効期間切れ：`duduclaw-spawnd` が動作し停止していないか確認）、または別のインストール済み世代を指している（更新後はゲートウェイを再起動）。 |
| `inner_prepare.registry.runtime_missing`、`.runtime_duplicate`、`.mcp_disabled`、`.script_executable` | 公開情報にこのランタイムの記録がない（または 2 件ある）、記録が `mcp: false`、または実行ファイルがスクリプト：インストール済みの署名ポリシーがこのランタイムをツール付きで提供していない。 |
| `inner_prepare.registry.acceptance_missing` | インストール済みバンドルにこのランタイムの MCP 受け入れ記録がない：`--accept-mcp` でビルドしたバンドルをインストールしてください。 |
| `inner_prepare.registry.acceptance.<check>`（`runtime`、`version`、`runtime_digest`、`bridge_digest`、`projection_digest`、`inherited_fd`、`initialize_exit`、`parent_exec_exit`、`mcp_child_exec_exit`、`exclusive_config_exit`、`transcript_digest`）、`inner_prepare.registry.projection_invalid`、`inner_prepare.registration_runtime`、`inner_prepare.projection` | インストール済みバンドルの MCP 受け入れ記録が公開されたランタイムと一致しない（別のランタイムのバイナリ、バージョン、ブリッジ、または別リリースのポリシーツールで受け入れた）。一致するバンドルを再インストールしてください。そのバンドルに `duduclaw-isolation-policy verify` を実行すると同じ確認が示されます。 |
| `inner_prepare.issue.live_snapshot` | 実行用権限の発行時の即時の再確認が失敗した（原因は上の `recheck.*` と `live_snapshot` と同じ）。 |
| `inner_prepare.issue.not_reserved`、`.reservation`、`.expiry`、`.time` | 隔離サービスがこの予約をもう予約中と報告しない、または予約が期限切れ（システム時計のずれの後にも表れます）。 |
| `inner_prepare.issue.principal`、`.binding`、`.policy_revision`、`.agent_slug`、`.binding_principal` | ターンで取得した権限が現在の権限と異なる（その人、社員の所属、ポリシーが途中で変わった）、またはその人がこの社員の所属のもとで行動できない。 |
| `inner_prepare.issue.grants` | このターンのリソース権限（ワークスペース、コネクタ、Web の接続先）がこの社員には認められない。 |
| `inner_prepare.issue.encode.<rule>`（`resource_mode`、`interval`、`duplicate_grant`、`other`） | 実行用権限をエンコードできない：`interval` — ターンの権限が実行用権限の発行前に期限切れになった（またはシステム時計のずれ）。その他は不具合（報告してください）。 |
| `inner_prepare.issue.random`、`.signer`、`.state_lock`、`.capacity`、`.duplicate`、`inner_prepare.employee_mismatch`、`.prepared_missing`、`.state_lock`、`.pending_duplicate`、`.argv`、`.clock`、`.registry.unreported` | OS の乱数源またはシステム時計の失敗、または不具合（報告してください）。 |
| `inner_prepare.verify_blocking` | 上の `verify_blocking` と同じ（ツール境界のブロッキング確認）。 |

実行の開始後、ツール境界はその MCP エンドポイントを有効にします。ここで拒否されると `isolated launch activation refused`（固定の `step`：`inner_activate.<substep>` のみ）が記録され、続いて `isolated spawn refused locally step="launch_activate"` が記録されます。ターンの site は `launch.activate.<step>` です。開始済みの実行は取り消され、実行用権限も無効化されます。

| `step` | 確認すること |
| --- | --- |
| `inner_activate.mcp_fd_missing`、`inner_activate.mcp_receipt.<class>` | 隔離サービスがこの実行の MCP エンドポイントを渡さなかった（実行が早く終了した、または I/O の受け渡しがない）。`duduclaw-spawnd` のジャーナルを確認してください。 |
| `inner_activate.bridge.registry.*` | 上の `inner_prepare.registry.*` と同じ（開始後に読み直したもの）。 |
| `inner_activate.bridge.publish.*`（上の `registry` の名前）、`.publish.policy`、`.publish.derive`、`.publish.record`、`.publish.registration_changed` | 準備から有効化までの間に公開情報が変わった（更新または利用権の変更）、またはこの社員にこのランタイムがもう提供されていない。 |
| `inner_activate.bridge.publish.run_receipt_read` | root の実行ごとの MCP 受領記録 `/run/duduclaw-spawnd/mcp-<run>.json` がない、別の起動のもの、または 60 秒より古い。`duduclaw-spawnd` のジャーナルを確認してください。 |
| `inner_activate.bridge.publish.receipt.<group>`（`generation`、`run`、`employee`、`runtime`、`digests`、`projection_digest`、`mount`、`acceptance`）、`inner_activate.bridge.receipt_binding`、`.bridge.registration_runtime`、`.bridge.projection`、`inner_activate.projection_digest` | マウントしたツール設定の root による読み戻しが、この実行用に準備したものと一致しない。一致するバンドルを再インストールし、続く場合は報告してください。 |
| `inner_activate.bridge.platform` | Linux ではない。 |
| `inner_activate.boundary_activate.live_mismatch`、`.live_snapshot`、`.time` | エンドポイントが届いた時点で実行がもう動作中でない、その人の権限が変わった、即時確認が失敗した、または実行用権限が期限切れ。 |
| `inner_activate.delivery`、`.receipt_control`、`.boundary_activate.delivery`、`.boundary_activate.unknown`、`.boundary_activate.binding`、`.boundary_activate.revoked`、`inner_activate.pending_missing`、`.revoked_after`、`.active_duplicate` | エンドポイントがこの実行の予約のものではない、または途中で実行が取り消された。それ以外は不具合。 |
| `inner_activate.boundary_activate.random`、`.boundary_activate.state_lock`、`inner_activate.state_lock`、`.clock`、`.verify_blocking` | OS の乱数源、システム時計、ブロッキング確認のプールの失敗、または不具合。 |

有効化の後、ゲートウェイは実行が解放されるまでその MCP エンドポイントを提供します。その前にセッションが異常終了すると、ゲートウェイは警告 `isolated MCP session ended` を 1 件記録します。フィールドは固定の `step` のみです（識別子・パス・内容は含みません）。正常に終わった実行（ランタイムがリクエストの合間に MCP を閉じ、その後実行が解放される）は何も記録しません。この行はセッション自身のタスクから出るため、ターンの `isolated run refused` の行はこれを site として持ちません。2 行を突き合わせて読んでください。API ワーカーのターンが `reason="MCP disconnected"` または `"MCP authority changed"` で拒否された場合、たいていこの行で原因が分かります。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (MCP session ended|run refused)'
```

| `step` | 確認すること |
| --- | --- |
| `second_request` | ランタイムが前のリクエストへの応答前に次のリクエストを送った（そのリクエストの取り消し通知以外）。このリリースより前は、`notifications/initialized` の直後に送られる `tools/list` によって、保護された API ワーカーの実行はすべてこの形で終了していました。再発した場合は報告してください。 |
| `peer_closed_before_request` | ランタイム側から何も届かなかった：ランタイムが MCP の開始前に失敗した、`/runtime/duduclaw-mcp-bridge` を起動できなかった、またはディスクリプタ 3 が引き継がれなかった。ターンの `isolated run refused` の行（`reason`、`exit`）がランタイム側の状況を示します。 |
| `peer_closed_in_request` | リクエストの処理中または応答中にランタイムが閉じた：終了した、取り消し・タイムアウトになった、またはブリッジが呼び出しの途中で終了した。 |
| `revoked` | この実行の実行用権限が、1 秒ごとの巡回（実行の即時確認が失敗）、制御接続の喪失、またはゲートウェイのツール境界の停止によって無効化された。実行自身の解放ではこの行は記録されません。 |
| `context.live_mismatch`、`context.live_snapshot`、`context.time`、`context.expired_after`、`context.revoked` | 実行の権限の即時再確認（リクエストごと、および 100 ミリ秒ごと）が拒否した：隔離サービスがもう実行中と報告しない、その人の権限・社員の所属・ポリシーが変わった（上の `recheck.*` と同じ原因）、状態の取得が失敗したか 250 ミリ秒を超えた、または実行用権限の有効期間が終わった。 |
| `context.serial_wait`、`context.blocking`、`context.clock` | 即時再確認が 2 秒を超えた、ブロッキング確認のプールが 2 秒間空かなかったか確認が 10 秒を超えた（`users.db`／`apply.db`、root の読み戻し、状態応答が遅い）、またはシステム時計の失敗。ホストの負荷と `duduclaw-spawnd` のジャーナルを確認してください。 |
| `admission_timeout`、`readiness` | 最初の即時確認がリクエストの上限（305 秒）を超えた；社員のツールサービスがこの実行の実行用権限に対して揃っていない。 |
| `frame_invalid`、`frame_too_large`、`frame_partial`、`window_limit` | ランタイムが 1 行 1 件の JSON-RPC 2.0 メッセージ以外を送った、1 行が 256 KiB を超えた、行が途中で切れた、または 60 秒間に 120 件を超えた。 |
| `dispatcher.<class>`（`invalid`、`invalid_time`、`revoked`、`binding_mismatch`、`unavailable`） | ツールディスパッチャーがメッセージを拒否した（例：ツール引数の形式誤り、未対応の通知、実行のターンをもう解決できない）。末尾はその分類です。 |
| `request_timeout` | 1 回のツール呼び出し（承認待ちを含む）が 305 秒を超えた。 |
| `response_encode`、`write_timeout`、`write_failed`、`read_failed` | 応答が 4 MiB を超えた、ランタイムが 5 秒間応答を読まなかった、またはソケットの失敗。 |
| `socket_missing`、`socket_setup`、`agent_slug`、`subscribe` | セッションを開始できなかった（その間に実行が無効化された）、または不具合。 |

保護モードでは、拒否された作業の原因を文面から判定できない場合、末尾の理由コードは `unknown` ではなく `safety_gate` になります。

## 更新とアンインストール

独立した更新サービスはまだインストールできません（今後のバージョンで提供予定）。現在のホストは上記の単一実行ファイルの節に従って更新してください。移行と更新サービスの完全な準備後、Admin はダッシュボードで安定版を要求するか、インストール済み CLI を使います。

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
| UID／アカウント／SSH／IAM 失敗 | 予約範囲の衝突、別名、補助グループ、ログイン／cron／at、linger を解決。passwd や group に `files`／`systemd` 以外の NSS モジュール（SSSD や GCE OS Login など）を使う構成はサポート対象外。sshd のあるホストで特定の社員が準備完了にならない場合：journal の `SSH denial probe duduclaw-emp-<uid>: …` と `journalctl -u 'duduclaw-isolation-probe@*'` を確認。`DenyUsers` のドロップイン設定は削除しないこと。 |
| ストレージ／クォータ失敗 | 下位デバイス構成、強制クォータ、空き容量、ホスト予約容量を確認。ディスク使用量ポーリングで代替しない。 |
| DNS／egress 失敗 | root が選んだ resolved Varlink／DNS broker 経路を確認。egress に私設 resolver IP の例外を追加しない。DNS の準備状態（`host_dns_route_unverified`）はインストーラーのレシートではなく spawnd 自身のライブ検査です。spawnd は約 1 分ごとに、署名済みポリシーの `dns_probe_name` をその経路で解決するよう egress に依頼し、公開アドレスが 1 つ以上得られた場合のみ合格とします。署名済みポリシーが v4 より古い場合、egress が動いていない場合、このホストからその名前を解決できない場合は未検証のままです（外向き DNS と egress ジャーナルの `dns_probe` 行を確認）。 |
| 解析 root 失敗 | 両 root の所有者、モード、予期しない内容を調査。ホスト `/etc` やデータをコピーしない。 |
| 移行プラットフォーム／設定リンクなし | 既存環境を保持し、完全な検証済み連携を取得。旧権限データのコピーや移行ゲート迂回をしない。 |
| 確認／交換待ち | 移行の確認で対応項目の確認、再試行、新しい認証情報設定を完了。承認だけでは移行完了にならない。 |
| 更新受理済みだが未 committed | 永続的な要求状態と準備エラーを追跡し、手動バイナリ交換を並行実行しない。 |

### spawnd ジャーナルのアカウント検証行

`registry_verified`、`accounts_locked`、`ssh_live_negative_checks`、`cron_at_acl_and_jobs_verified`、`external_iam_reservation_verified` がまとめて失敗する場合、spawnd のジャーナルに失敗したステップが出力されます。各行は 1 回だけ出力され、ステップまたはアカウントが変わったときだけ再出力されます。

```text
duduclaw-spawnd: employee account verification failed: step=<label> account=<duduclaw-emp-NNNNN|->
duduclaw-spawnd: allocation failed: step=<label>
```

1 行目はホスト全体のアカウント検査から出力されます。この検査は社員範囲内のすべての `duduclaw-emp-*` アカウントを対象とし、退役済み（tombstone）のアカウントも含みます。`account=-` はホスト単位のステップを表します。2 行目は社員の割り当てが失敗するたびに 1 回出力されますが、同じ label は 10 秒間に最大 5 回までです。それ以降の同じ label の失敗は数だけ数えられ、その 10 秒が終わったときに `duduclaw-spawnd: allocation failed: step=<label> suppressed=<count>` の 1 行で報告されます。SSH プローブの結果待ちと保留中予約の上限到達は失敗ではないため、出力されません。label は固定の識別子で、どちらの行にもファイル内容やエラー文は含まれません。準備状態の出力と `readiness.json` の検査名は変わりません。

| label | 失敗した内容 |
| --- | --- |
| `login_defs`、`subids` | `/etc/login.defs`、`/etc/subuid`、`/etc/subgid` のいずれかが読めない、または形式が不正。 |
| `registry_rows` | 社員登録がホストの uid 範囲と合わない、または再開できない予約がある。 |
| `nss_modules` | passwd または group が `files`／`systemd` 以外の NSS モジュールを使っている。 |
| `enumerate` | `getent passwd`／`group` が読めない、または社員範囲に別名や外部のエントリがある。 |
| `passwd_row`、`shadow_row`、`groups` | アカウントの passwd 行（`/nonexistent`、`/usr/sbin/nologin`）、shadow 行（`!*`、期限切れ）、グループ一覧（自身の gid のみ）のいずれかが正しくない。 |
| `cron_acl`、`at_acl` | `/etc/cron.deny` または `/etc/at.deny` がアカウントを拒否していない、または allow ファイルで上書きされている。 |
| `cron_spool`、`cron_tables` | アカウントのユーザー crontab がある、または `/etc/crontab`、`/etc/cron.d` にそのアカウントで実行される行がある（そこに読めないファイルや root 所有でないファイルがある場合も失敗）。 |
| `at_spool` | at の spool ディレクトリ（`/var/spool/cron/atjobs`、`/var/spool/cron/atspool`、`/var/spool/at`、`/var/spool/at/spool`）にアカウント所有のエントリがある、ディレクトリはあるが読めない、または一覧にあったエントリが所有者の確認前に消えたか確認できない。atd はジョブの開始時にそれを `atjobs` から削除するため、エントリが消えても無害とは扱いません。次の更新で再検査されるので、他のアカウントのジョブが終わったなどの一時的な変化はそこで解消します。 |
| `linger`、`sessions` | linger ファイルがある、またはアカウントの `user@<uid>.service` か logind セッションが動いている。 |
| `ssh_config`、`ssh_probe` | `sshd -t`／`sshd -T` がアカウントを拒否していない、または port がない。あるいは SSH 拒否プローブに Denied の結果がない。 |
| `userdb_service`、`userdb_user`、`userdb_group` | `/run/systemd/userdb` が読めない、または userdb のユーザー／グループレコードが社員の名前空間に触れている。 |
| `local_identity`、`candidate`、`owner_inventory` | 割り当て時：新アカウントの ID や別名が既にある、候補 uid の検索に失敗した、または spawnd のファイル所有者一覧の作成に失敗した。 |
| `login_barriers`、`create_group`、`create_user`、`lock_user`、`quarantine` | 割り当てまたは復旧時：deny エントリや sshd ドロップイン、groupadd、useradd（またはそれが作ったメール spool）、usermod／chage／loginctl によるロック、アカウントのプロセス停止のいずれかが失敗した。 |
| `reserve`、`create_account`、`readback` | より細かい label のない割り当てステップが失敗した。たとえば `reserve` は社員 uid 範囲を使い切ったことを表す。 |
| `existing_row`、`persist_reservation`、`persist_final`、`store_poisoned`、`clock` | 割り当て時：社員に退役済みまたは再開できない登録行がある、アカウント作成前または結果確定後の登録の書き込みに失敗した、このプロセスで以前に書き込みが失敗したため登録ストアが書き込みを拒否している、または単調時計を読めない。 |
| `unlabelled` | spawnd のラベル付き検査の外から来たエラー。割り当てでは出力されない。 |

`at` パッケージは削除不要です。spawnd は root として spool を直接読み、`/usr/bin/at` を実行しません。行を消すために deny ファイル、spool、アカウントを手で編集しないでください。原因を直すと、次の更新で `evidence complete` が出力されます。失敗した割り当ての uid は永久に退役し、提案は「失敗」のままです。原因を直した後、移行の確認で失敗項目の補償を再試行してください。その後の新しい提案には次の uid が割り当てられます。

### spawnd ジャーナルの起動拒否行

ゲートウェイが `isolated run not started`（たとえば `site="spawn.backend.unavailable"`）を記録した場合、spawnd のジャーナルにはその拒否された起動ごとに 1 行が出力されます。

```text
duduclaw-spawnd: start refused: step=<label>
```

この行には社員 ID、実行 ID、パス、リクエスト内容は含まれません。ゲートウェイに返すエラーコードは変わりません。受け入れゲートの label は `ReserveRun`、`StartRun`、キュー中の実行の受け入れのいずれからも出力されます。

この行は label ごとに出力頻度が制限されます。10 秒間に同じ label の拒否は最初の 5 件だけが出力されます。それ以降の同じ label の拒否は数だけ数えられ、その 10 秒が終わったときに件数を示す 1 行が代わりに出力されます。

```text
duduclaw-spawnd: start refused: step=<label> suppressed=<count>
```

すべての拒否は、出力されるか `suppressed=` の件数に含まれます。出力された行数とこれらの件数の合計は、拒否された起動の数と一致します。

| label | 起動を拒否した内容 |
| --- | --- |
| `frame`、`peer`、`request`、`fds`、`run_id` | 制御ソケット：デコードできないフレーム、ゲートウェイ以外の接続相手、自身の検証に失敗したリクエスト、リクエストと合わないディスクリプタ、またはランダムな実行 ID を作れない。 |
| `clock`、`update_drain`、`stopping`、`not_reserved`、`reservation`、`pty`、`queue_full`、`launch_policy`、`input_bytes`、`input_fds`、`internal` | 受け入れ：時計が戻った、更新のドレインが閉じている、この社員の停止が進行中、予約がない／誤り／期限切れ、PTY の不一致、キューが満杯、起動ポリシーが不正、または入力の予算に空きがない。 |
| `employee_not_admitted` | 社員が spawnd の登録で Ready 行になっていない。 |
| `not_ready(<check>)`、`report_stale`、`report_invalid`、`pty_budget`、`landlock_evidence`、`protocol`、`boot_clock`、`policy_missing`、`poisoned` | ホストの準備状態：名前の readiness.json 検査が false、証拠レポートが 1 秒より古いか別のブートのもの、PTY 予算や Landlock の証拠がない、検証済みポリシーが読み込まれていない、または spawnd の再起動が必要。 |
| `scan_stale`、`scan_failed`、`scan_busy`、`intruder`、`view_busy` | UID モニターのスキャンが古い／失敗／ロック中、実行外のこの社員のプロセスを検出した、または受け入れビューがロック中。 |
| `account_unknown`、`ssh_pending`、`account(<step>)` | 社員の登録行がない、SSH 拒否プローブの結果がまだない、またはアカウント検証が `<step>`（上の表の label）で失敗した。 |
| `entitlement_missing`、`runtime_not_entitled`、`runtime_ambiguous`、`pty_not_allowed`、`turn_limit` | 権利付与：社員の権利付与がない、要求されたランタイムの付与済みプロファイルがないか 2 つある、PTY が許可されていない、またはステップ上限が署名された範囲外。 |
| `queue_timeout(<blocker>)`、`queue_cancelled(<blocker>)` | キュー中の実行が `<blocker>` に止められたまま期限切れになった、またはゲートウェイに取り消された：受け入れの label、`host_runs`、`employee_busy`、`host_ptys`、`cleanup`、`cleanup_incomplete`。 |
| `uid_monitor_register`、`worker_mailbox`、`run_fence`、`cleanup`、`cleanup_incomplete`、`storage_usage`、`disk_quota`、`run_journal`、`storage_attach`、`storage_receipt`、`workspace` | 起動（unit 作成前）：モニター登録、ライフサイクルのメールボックス、取り消された実行、クリーンアップの読み戻し、ストレージ使用量やクォータ、実行ジャーナル、イメージのアタッチやその受領、ワークスペース。 |
| `mcp_channel`、`egress_channel`、`egress_register`、`output_relay`、`stdin_pipe`、`supervisor_identity`、`nonce`、`bootstrap_plan`、`uid_monitor`、`mcp_projection`、`anchors`、`mounts`、`mcp_record`、`native_profile`、`helper_mounts`、`seal`、`trampoline_prepare`、`bootstrap_invalid`、`unit_policy` | 起動（unit 作成前）：チャネルやパイプ、egress サービスへの登録（不明なプロファイル、または egress 制御接続がない）、bootstrap 計画、MCP 投影、公開アンカー、マウント、ネイティブプロファイル、trampoline、unit ポリシー。 |
| `unit_start`、`unit_cgroup`、`bind_unit`、`launch_poll`、`startup_timeout` | systemd が unit を拒否した、unit の cgroup や trampoline の結び付けに失敗した、Running 前に起動が失敗した、または起動予算内に Running にならなかった。 |
| `host_gate`、`host_policy`、`host_start` | テスト用アダプターのみ。 |

このリリースより前は、割り当てジャーナルで Ready の社員に `employee_not_admitted` が出るのは不具合でした。spawnd は受け入れ対象の社員を起動時にしか読まなかったため、spawnd の起動後に割り当てた社員は spawnd を再起動するまで拒否され続けました。現在の spawnd は、割り当て、権利付与、無効化、退役に応答する前に、受け入れ対象の社員を公開します。割り当てや権利付与の応答後に送られた `ReserveRun` は、更新後の社員と権利付与で検査されます。無効化や退役の応答後は、その社員は拒否されます。

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

ランチャーは実行ファイルと全祖先ディレクトリ、自身の配置先が root 所有で他のユーザーに書き込みを許さないことを確認します。アカウントを検査して同 uid の残存プロセスを終了した後に鍵を開き、uid/gid を変更し、補助グループと全 capabilities を削除して `no_new_privs` を設定します。gateway は鍵を受け取る前に non-dumpable にし、systemd の親プロセス、正確な cgroup v2 行（`0::/system.slice/duduclaw-gateway.service`、またはユニットが `Slice=duduclaw-tcb.slice` を持つ場合の `0::/duduclaw.slice/duduclaw-tcb.slice/duduclaw-gateway.service` のみ。隔離構成のユニットは後者）、実行ファイルとプロセスの権限、自身から鍵のパスを開けないことを検証します。ネイティブインストーラー、鍵を開く前のランチャー、Rust の検証はいずれも初期以外の user namespace を拒否します。`/run/.containerenv`、`/run/systemd/container`、PID 1 の環境内の `container=` も拒否理由です。root のインストーラーとランチャーは `/proc/1/environ` を読める必要があり、非特権 gateway は権限不足の場合だけその読み取りを省略できます。コンテナでは従来のコンテナ規則を適用し、モード変数でネイティブ保護を取得することはできません。

既定の `revoke` ポリシーでは、既知の Docker／Podman／containerd ソケットは存在しないか、アクセス不能でなければなりません。書き込みアクセス検査と非ブロッキング接続の両方が拒否される必要があります。`DOCKER_HOST` は設定しないでください。独自 slice、cgroup v1／混在方式、想定外の sudo 出力、結果不明の検査はすべて拒否します。理由は journal または認証情報カードに表示されます。権限を緩めて回避しないでください。

ユニットには次の設定も適用します。

```ini
InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
LockPersonality=yes
SystemCallArchitectures=native
```

[systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) の仕様に従い、各 `-` 接頭辞は存在しないパスを無視します。一般的なエンジンの接続先を隠し、ハードウェアを隔離し、カーネル／モジュール／cgroup の変更を禁止して、実行モードと syscall アーキテクチャを制限します。`RestrictSUIDSGID` は意図的に設定しません。systemd 255 ではすべての `openat2` 呼び出しが失敗し、gateway は `openat2` のパス解決制限でファイルを開くためです。ランチャーは set-ID ファイルを作成せず、gateway は `no_new_privs` で動作します。`RestrictNamespaces` は設定しません。gateway の Codex／bwrap タスクサンドボックスは user、mount、PID、network namespace を必要とします。

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
