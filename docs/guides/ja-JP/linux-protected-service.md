# ネイティブ Linux サービスで個人アカウントを接続する

> 以下のインストールとユーザーデータ移行は既存 v1.79.3 サービスの手順です。開発中の更新経路は別の未完成実装です。既存環境を変更する前に[更新とアンインストール](#更新とアンインストール)を確認してください。

公式 Linux CLI バイナリには **glibc ≥ 2.31** が必要です。Ubuntu 20.04+、Debian 11+、RHEL/Rocky Linux 9+ をサポートします。古いシステムでは Docker を使用してください。この基準は CLI/gateway（デスクトップ版の CLI sidecar を含む）に適用され、デスクトップ GUI には別途 WebKitGTK などのシステムライブラリが必要です。

このモードでは、ネイティブ Linux の「自分の接続」で個人 Google とリモート MCP OAuth を利用できます。gateway は専用の `duduclaw` アカウントで動作し、起動時に root が用意した鍵を受け取ります。通常のユーザーサービスには鍵を渡しません。[個人 Google 接続](google-personal-connections.md)も参照してください。

## ホストの準備

PID 1 が systemd、単一の cgroup v2 階層を使う Linux が必要です。GNU coreutils、GNU tar、curl、minisign、`getent`、`id`、`useradd`、util-linux の `setpriv` を用意してください。AI セッションやコンテナの外で、人間の運用者が端末から sudo でインストールします。初期 user namespace が必要です。`/proc/self/uid_map` と `/proc/self/gid_map` はそれぞれ `0 0 4294967295` の 1 行だけを含む必要があります（空白幅は任意）。Rootless コンテナや ID を再マッピングした user namespace は拒否します。

専用アカウントの uid/gid は 0 以外、シェルは `nologin` または `false` とし、sudo のコマンド権限と linger を与えないでください。`sudo`、`wheel`、`admin`、`adm`、`docker`、`lxd`、`incus`、`libvirt` グループへの所属は禁止です。`systemd-journal` は許可されます。他のサービスとこのアカウントを共用しないでください。鍵を渡す前に、同じ uid のゾンビ以外の全プロセスを終了します。

## インストールと起動

`X.Y.Z` を公開済みバージョンに置き換えてください。`/opt` と全祖先ディレクトリが root 所有で、他のアカウントに書き込みを許さないことを確認します。以下の sudo コマンドは、root 所有・0700 の `/opt` 内の一時ディレクトリに公式アーカイブと `.minisig` を取得し、公開鍵でアーカイブ全体のバイト列を検証してから実行ファイルを展開します。検証済みファイルだけを、その root シェル内で実行します。Linux の配布名は `duduclaw-linux-x64.tar.gz` と `duduclaw-linux-arm64.tar.gz` で、それぞれ同名の `.minisig` が付属します。

```sh
sudo /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin /bin/sh -eu <<'ROOT'
version=X.Y.Z
case "$(uname -m)" in
  x86_64) asset=duduclaw-linux-x64.tar.gz ;;
  aarch64|arm64) asset=duduclaw-linux-arm64.tar.gz ;;
  *) echo 'Unsupported architecture' >&2; exit 1 ;;
esac
umask 077
stage=$(mktemp -d /opt/duduclaw-install.XXXXXXXX)
trap 'rm -f -- "$stage/$asset" "$stage/$asset.minisig" "$stage/duduclaw"; rmdir -- "$stage"' EXIT
cd "$stage"
base="https://github.com/zhixuli0406/DuDuClaw/releases/download/v$version"
curl -q --proto '=https' --tlsv1.2 -fL "$base/$asset" -o "$asset"
curl -q --proto '=https' --tlsv1.2 -fL "$base/$asset.minisig" -o "$asset.minisig"
minisign -Vm "$asset" -x "$asset.minisig" \
  -P 'RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh'
tar -xzf "$asset" --no-same-owner --no-same-permissions duduclaw
chown root:root duduclaw
chmod 0755 duduclaw
"$stage/duduclaw" service install --protected-linux --enable-now
ROOT
sudo systemctl status duduclaw-gateway.service
sudo journalctl -u duduclaw-gateway.service -n 100 --no-pager
```

ユーザーが書き込める npm インストール先やホームから sudo で実行しないでください。カーネルが root として起動する前に、AI 社員がプログラムを置き換える可能性があります。実行中ファイルの解決先や祖先が root 所有でない、または非 root が書き込める場合、インストーラーは警告し `--i-verified-this-binary` を要求します。この確認では、すでに root で実行された不正なファイルを安全にできません。上記の検証・展開手順を使用してください。

`--enable-now` を省略するとインストールだけを行います。後から `sudo systemctl enable --now duduclaw-gateway.service` で起動できます。ダッシュボードでセットアップを済ませ、「セキュリティ → 認証情報の保護」を有効にしてください。「独立した鍵（AI 社員は読み取れません）」を確認してから個人アカウントを接続します。CLI プロセスには gateway の鍵を渡さないため、端末の認証情報ステータスは gateway の状態を示しません。

root 所有の実行ファイルは `/opt/duduclaw/bin/duduclaw`、組み込みランチャーの配置先は `/usr/local/libexec/duduclaw/protected-launcher`、ユニットは `/etc/systemd/system/duduclaw-gateway.service` です。データは `/var/lib/duduclaw/.duduclaw`、ルートキーは `/var/lib/duduclaw-protected/root.key`（root:root、0400、リンク数 1、32 バイト）に置きます。ユニットは `KillMode=control-group` を使用し、システムを読み取り専用にしてデータと鍵のディレクトリだけを書き込み可能にします。

## ユーザーインストールから移行する

旧データをバックアップし、旧 gateway を停止します。旧ユーザーサービスの所有者として実行してください。

```sh
systemctl --user stop duduclaw.service
systemctl --user disable duduclaw.service
sudo /opt/duduclaw/bin/duduclaw service install --protected-linux \
  --migrate-from "$HOME/.duduclaw" --enable-now
```

先に上記の検証済みインストールを完了してください。初回起動前に移行する場合、インストール時は `--enable-now` を省略します。新 gateway がすでにデータを作成した場合はバックアップを保持し、置き換え時に `--force-migrate` を明示してください。移行は元のユーザー自身の端末から実行し、root ログインシェルは使いません。`SUDO_UID` と `SUDO_GID` は存在し、有効かつ 0 以外である必要があります。`getpwuid` でその UID のホームを取得し、その `.duduclaw` だけを受け入れます。指定パスの正規化結果が一致し、全ソースディレクトリとファイルが `SUDO_UID` の所有である必要があります。`/var/lib/duduclaw-protected`、`/var/lib/duduclaw`、`/etc`、`/root`、`/proc`、`/sys` 以下は拒否します。ファイル記述子を固定する `openat` の走査では、祖先も含めシンボリックリンクを拒否します。

インストーラーが別ユーザーの代わりに最初の 2 コマンドを実行することはありません。そのディレクトリを `/var/lib/duduclaw/.duduclaw` にコピーし、専用アカウントの所有にします。移行先が存在する場合は `--force-migrate` を明示しない限り拒否します。`--force-migrate` は旧移行先を `/var/lib/duduclaw/.duduclaw.previous-<UUID>` として保持します。シンボリックリンク、ハードリンク、特殊ファイルは拒否します。置き換える前にシステムサービスを停止し、バックアップを保存してください。設定内の絶対パスも確認します。`ProtectHome=yes` により旧ユーザーのホーム以下にはアクセスできません。

認証情報の保護を再設定し、各利用者に個人 Google とリモート MCP を再接続してもらってください。旧暗号文は別の鍵に結び付いており、新環境の鍵では復号できません。AI runtime は root 管理のシステムパスに配置してください。旧ユーザーの CLI、ログインファイル、シェル起動ファイルは自動的には引き継ぎません。

## 更新とアンインストール

**開発ブランチ：保護された更新はまだ導入できません。** ダッシュボードと CLI は、独立した root 更新サービスへ要求を送る方式に変わりました。隔離移行パッケージによるサービス、アカウント、信頼ルートの準備が必要です。v1.79.3 の実行ファイルや timer だけを置き換えても準備は完了しません。

Admin はダッシュボードから新しい安定版を要求し、永続化された要求 ID で結果を追跡できます。CLI ではバージョンの明示が必要です。

```sh
sudo duduclaw service upgrade --protected-linux --version X.Y.Z
```

更新サービスの socket に渡すのはバージョンと要求 ID だけです。呼び出し側はパス、URL、コマンド、信頼鍵を指定できません。サービスは相手の身分、署名付きマニフェスト、ファイルのハッシュ、アーキテクチャ、有効期限、バージョン下限と連番を検証します。受理、ダウンロード、ステージングはインストール成功を意味しません。応答が途切れてもダッシュボードは ID を保持して追跡し、`committed` だけを成功として表示します。サービスに接続できない場合、旧インストーラには戻りません。

Linux アダプタは世代を切り替え、固定サービスを再起動し、実際のバージョン・プロトコル・実行ファイルを照合した後に下限を更新します。中断された処理を再開し、準備完了を確認できない場合は作業受付を閉じたままにします。パッケージ間の排出・移行／runtime・準備完了エンドポイントの接続は必要で、不足すれば更新を拒否します。移行 bootstrap の二重署名手順、実物の FIDO2 署名、Linux 実機の更新受け入れ試験も未完了です。このソースの無人更新を有効にしないでください。

既存の **v1.79.3 インストール**は、検証済みの固定した v1.79.3 実行ファイルと対応する運用手順を維持してください。その旧版は minisign 署名付きアーカイブを検証し、実行ファイルを原子的に置き換え、稼働中のサービスを再起動します。ダッシュボードは手動更新のみです。root 管理の `version-floor` と `version-pending` は通常のダウングレードを防ぎ、中断した対象を記録します。これらは新しい更新サービスの状態・信頼ストアではありません。

既存の v1.79.3 のアンインストールコマンドは次のとおりです。

```sh
sudo /opt/duduclaw/bin/duduclaw service uninstall --protected-linux
```

旧システムサービスを停止・無効化し、unit、実行ファイル、ランチャーを削除します。root 鍵を含む `/var/lib/duduclaw` と `/var/lib/duduclaw-protected` は**削除せず**、旧バージョン下限と保留記録だけを除去します。このコマンドを新しい隔離インストールの削除・復旧手順として使わないでください。導入前に、移行パッケージによる新環境の移行・復旧手順が必要です。

## 自動更新（開発用のみ）

現在の `scripts/linux-protected/auto-upgrade/` は既存 timer の時刻を維持します。現地時刻 03:30 に最大 20 分のランダムな遅延を加え、`Persistent=false` により朝の起動時に未実行分を補いません。helper は固定の root 管理コマンド `/usr/libexec/duduclaw-updated-launch --check-latest` を呼びます。`--dry-run` は候補の照会だけを行い、Apply を送りません。

候補照会は非特権の `duduclaw-dl` アカウントで curl を実行し、継承した設定を消去します。API 応答は 1 MiB 以下で、JSON 全体を解析します。最新リリースの記録には `draft=false`、`prerelease=false`、安定版の形式が必要です。未署名の索引は候補の選択にしか使わず、インストールには root サービスによる公式署名付きマニフェストの検証が必要です。同じ版や古い版には要求を送りません。接続、アカウント、状態、サービスの異常は要求を停止し、旧経路には戻りません。要求側 unit のタイムアウトは 6 分で、リリースファイルを書き込みません。

この開発中 helper を v1.79.3 にコピーしたり、timer を有効にしたりしないでください。既存 v1.79.3 の夜間運用では、完全な隔離移行が提供されるまで、以前に確認した固定版 helper を維持してください。運用コードを移動する `main` から取得しないでください。

ソースのインストーラは引き続き root:root 所有、通常ファイル、他者が書き込めない祖先ディレクトリを検査し、symlink と sticky `/tmp` を拒否して root 専用スナップショットからインストールします。この検査はコードの出所を証明しません。将来の導入前にも正確な commit と root 管理のコピーを確認し、ユーザーが書き込める checkout から root インストーラを実行しないでください。[helper README](../../../scripts/linux-protected/auto-upgrade/README.md) を参照してください。

導入済みのスケジュールだけを削除して gateway とデータを残すには、確認済みインストーラのコピーを使います。

```sh
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

更新が進行中なら timer を無効化した後、削除を拒否します。完了を待って再実行してください。

## 保護範囲と制限

**機能制限：** 保護されたネイティブモードでは、`InaccessiblePaths` でコンテナエンジンを遮断し、`PrivateDevices` でデバイスを隔離します。そのため、コンテナを使うタスクサンドボックス、コンピューター操作、ハードウェアに依存する機能は利用できません。AI 社員がコンテナエンジンにアクセスできないようにするための意図的な制限です。

ランチャーは実行ファイルと全祖先ディレクトリ、自身の配置先が root 所有で他のユーザーに書き込みを許さないことを確認します。アカウントを検査して同 uid の残存プロセスを終了した後に鍵を開き、uid/gid を変更し、補助グループと全 capabilities を削除して `no_new_privs` を設定します。gateway は鍵を受け取る前に non-dumpable にし、systemd の親プロセス、正確な cgroup v2 行 `0::/system.slice/duduclaw-gateway.service`、実行ファイルとプロセスの権限、自身から鍵のパスを開けないことを検証します。ネイティブインストーラー、鍵を開く前のランチャー、Rust の検証はいずれも初期以外の user namespace を拒否します。`/run/.containerenv`、`/run/systemd/container`、PID 1 の環境内の `container=` も拒否理由です。root のインストーラーとランチャーは `/proc/1/environ` を読める必要があり、非特権 gateway は権限不足の場合だけその読み取りを省略できます。コンテナでは従来のコンテナ規則を適用し、モード変数でネイティブ保護を取得することはできません。

既知の Docker／Podman／containerd ソケットは存在しないか、アクセス不能でなければなりません。書き込みアクセス検査と非ブロッキング接続の両方が拒否される必要があります。`DOCKER_HOST` は設定しないでください。独自 slice、cgroup v1／混在方式、想定外の sudo 出力、結果不明の検査はすべて拒否します。理由は journal または認証情報カードに表示されます。権限を緩めて回避しないでください。

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

起動時と 60 秒ごとに、一般的なエンジンソケットへの到達性と補助グループを検査します。定期検査に失敗すると保持中の鍵をその場でゼロ化し、保護状態を拒否に変更して警告を記録します。個人接続は再起動まで利用できません。`/proc/net/unix` の待ち受け中の abstract socket も検査します。`/` で区切られた名前の要素が `docker`、`containerd`、`podman`、`buildkit`、`lxd` と完全一致するか、その名前の直後に `.`、`-`、`_` が続く場合に候補とします。任意の部分文字列では一致させません。 名前の一致だけでは拒否しません。短いタイムアウトの非ブロッキング接続で `SO_PEERCRED` を取得し、接続できた相手の uid が 0 の場合だけリスクと判断します。到達不能な候補と無関係な行の解析エラーは無視します。鍵の受け取り時に `/proc/net/unix` を読めない場合は拒否します。定期検査中の読み取り失敗では警告を記録し、その検査項目だけ以前の判定を維持します。パスとグループの検査は引き続き実行します。

AI 社員の Bash／ツールは gateway と同じ uid で動作します。保護は non-dumpable、root 所有の鍵とプログラム、sudo／linger がないこと、エンジンソケットへのアクセス拒否に依存します。社員は書き込み可能なデータを変更し、gateway を停止し、許可された操作を要求し、自分のターンで発行されたアクセストークンを読めます。ホストは `duduclaw` アカウントにコンテナエンジンや他の特権ブローカーへのアクセスを与えてはいけません。起動時と毎分の検査は一般的な接続先を対象とし、無条件の隔離を保証しません。未識別名の abstract socket、別パスのエンジンソケット、特権ブローカー、その他の権限昇格経路、ホスト root の侵害、gateway 内ですでに動いている悪意あるコードは対象外です。検査の間に新しいアクセス経路が生じる可能性もあります。実行ファイルの所有者検査はサービス uid による改変を防ぐもので、悪意ある root 管理者が渡すファイルの署名検証ではありません。

この鍵は汎用サンドボックスではありません。鍵を保持する間は gateway 内の ONNX 読み込み禁止も適用され、NER マスキング、ローカル embedding、Kokoro は使えません。必要に応じて正規表現のマスキングを使用してください。データとルートキーの両方を含むバックアップは認証情報を復号できます。ダッシュボードの保護キーだけを替えても旧データの復元は防げません。漏えいが疑われる場合、ホスト管理者はサービスを停止し、安全にルートキーを交換し、保護の再設定とアカウントの再接続を行ってください。

ネイティブ systemd のインストール、sudo の各バージョン、ソケット検査は対象ディストリビューションでの実測が必要です。単体テストや macOS のコンパイル成功を Linux 実機の検証と同一視しないでください。
