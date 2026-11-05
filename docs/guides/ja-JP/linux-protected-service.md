# ネイティブ Linux サービスで個人アカウントを接続する

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

## サービスアカウント用の AI CLI をインストールする

サービスは `duduclaw` アカウントで動作し、`HOME=/var/lib/duduclaw`、`PATH=/usr/sbin:/usr/bin:/sbin:/bin` を使用します。AI CLI はこれらのシステム PATH ディレクトリのいずれかにインストールし、サービスアカウントが読み取り・実行できる必要があります。`ProtectHome=yes` は `/home`、`/root`、`/run/user` を隠します。root が `/home/operator/.local/bin/claude` を見つけてもサービスからは利用できません。システムディレクトリから隠されたホームへのシンボリックリンクも使えません。

Claude Code では、実行前に `<ver>` をインストールするバージョンに置き換えてください。

```sh
cd / && sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>
sudo -u duduclaw -H claude auth login
```

`-H` と `--prefix /usr` は両方必要です。省略すると、元ユーザーの `~/.npmrc` の prefix 設定により CLI が再びユーザーのホームにインストールされる場合があります。Codex、Grok、AGY、Gemini を設定している場合も、サービスから読める同じシステム PATH にインストールし、各 CLI の手順でサービスアカウントとしてログインしてください。

`service install --protected-linux` と `service upgrade --protected-linux` の最後に、設定された主要推論 runtime と各社員の `agent.toml` が選択する runtime を検査します。移行時は移行先の設定を読みます。サービスアカウントとサービス PATH で実行権限を確認し、隠されたパスを除外します。元ユーザーのホームにしかない CLI はサービスで利用できないと表示します。主要 runtime がない場合は警告を表示し、非ゼロで終了します。後で runtime を設定する場合に限り、どちらのコマンドにも `--allow-missing-runtime` を指定できます。このフラグは CLI のインストールやログインを行いません。検査の失敗は、すでに行ったインストールやアップグレードを元に戻しません。

移行でコピーするのは `.duduclaw` のデータだけで、**AI CLI 本体やログイン認証情報は移行しません**。Claude があってもサービスアカウントが未ログインの場合、インストール時に上記のログインコマンドを表示します。元ユーザーのホームから認証情報をコピーせず、個別にログインしてください。gateway の起動時に設定された推論バイナリが見つからない場合は、修復手順を含む ERROR を記録します。runtime の状態と doctor にも保護サービス用の手順を表示します。

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

インストール済みの root 所有の実行ファイルから更新します。

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux
# Select a published version instead of latest:
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux --version X.Y.Z
```

`/proc/self/exe` が正確に `/opt/duduclaw/bin/duduclaw` を指し、そのファイルと全祖先が root 所有で非 root に書き込みを許さない場合だけ更新できます。公式 GitHub Release の Linux アーカイブ（既定は最新版）を取得し、updater と共通の組み込み minisign 公開鍵でダウンロードしたバイト列を検証してから、root 所有・0700 の一時ディレクトリに展開します。同じ配置先ディレクトリの一時ファイル、fsync、rename で原子的に置き換えます。更新中に新しいプログラムを root として実行することはありません。検証失敗時は既存プログラムを変更せず、ユニットは稼働中だった場合だけ再起動します。既存のデータとルートキーは維持します。このモードではダッシュボードは手動更新を案内し、プログラムの置き換えを拒否します。

```sh
sudo /opt/duduclaw/bin/duduclaw service uninstall --protected-linux
```

サービスを停止・無効化し、ユニット、実行ファイル、ランチャーを削除します。`/var/lib/duduclaw` と `/var/lib/duduclaw-protected`、その中のルートキーは**削除しません**。そのディレクトリからはバージョン下限と未完了の書き込み意図だけを削除します。データとキーは再インストール時に再利用できます。

更新は実行中のバージョンより新しい安定版 semver `X.Y.Z` のみ許可します。インストールと更新では `/var/lib/duduclaw-protected/version-floor` を下回るバージョンも拒否します。このファイルは root:root 所有、0600 で、実行ファイルの置き換え後に最高のインストール済みバージョンを原子的に記録します。ファイルがなければ実行中のバージョンを下限とし、不正な内容は拒否します。更新にロールバックオプションはありません。緊急時のロールバックは root が意図的にアンインストール（下限ファイルも削除）した後、古いアーカイブを使って上記の署名検証付き初回インストールを行います。データとルートキーは残ります。 置き換えが中断した場合も、root 専用の書き込み意図（`version-pending`）で検証済みの更新先を下限として保持します。同じ版か新しい版で署名検証付きインストールを再実行するか、意図的にアンインストールして解除してください。

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

起動時と 60 秒ごとに、一般的なエンジンソケットへの到達性と補助グループを検査します。定期検査に失敗すると保持中の鍵をその場でゼロ化し、保護状態を拒否に変更して警告を記録します。個人接続は、新たな起動時の鍵受け取り検査に成功するまで停止します。失効後も監視を続けます。自動復旧は次の節を参照してください。`/proc/net/unix` の待ち受け中の abstract socket も検査します。`/` で区切られた名前の要素が `docker`、`containerd`、`podman`、`buildkit`、`lxd` と完全一致するか、その名前の直後に `.`、`-`、`_` が続く場合に候補とします。任意の部分文字列では一致させません。 名前の一致だけでは拒否しません。短いタイムアウトの非ブロッキング接続で `SO_PEERCRED` を取得し、接続できた相手の uid が 0 の場合だけリスクと判断します。到達不能な候補は無視します。表は生のバイト列として読み取り、非 UTF-8 名が他のソケットの検査を妨げないようにします。すべての行を完全に解析し、不正なフィールドや flags、曖昧な名前、改行による断片があれば鍵の受け取りを拒否し、保持中の鍵も失効させます。表の読み取り失敗はどの状態でも検査失敗とし、復旧時の連続成功回数もリセットします。安全と判断する前に、必須の `NETLINK_SOCK_DIAG` dump で長さ付きの `UNIX_DIAG_NAME` を調べます。非エンジン名も含めて改行を拒否するため、改行の後が正しい形式の偽造行でも見逃しません。dump が取得不能、不正、中断、8 MiB 超過、250 ms のタイムアウトの場合も拒否し、カーネルには UNIX socket diagnostics の対応が必要です。どちらの検査も定期的な観測であり、全ソケットの原子的なスナップショットではありません。

`@docker_cli_` と 32 桁の小文字 16 進数に完全一致し、接続できる peer uid 0 の abstract socket は、必ずエンジンソケットと判定して鍵を失効させます。セキュリティ上の除外はありません。名前の識別は診断のヒントにだけ使用します。通常は、このホストで誰かが root として `docker` コマンド（例：`sudo docker compose …`）を実行したことを示します。実行中は個人アカウントを一時停止し、終了後に環境チェックが成功して自動復旧が利用できれば、通常は数分で復旧します。回避するには docker グループの一般ユーザーで docker を実行するか、開発用コンテナを別のホストで実行してください。

AI 社員の Bash／ツールは gateway と同じ uid で動作します。保護は non-dumpable、root 所有の鍵とプログラム、sudo／linger がないこと、エンジンソケットへのアクセス拒否に依存します。社員は書き込み可能なデータを変更し、gateway を停止し、許可された操作を要求し、自分のターンで発行されたアクセストークンを読めます。ホストは `duduclaw` アカウントにコンテナエンジンや他の特権ブローカーへのアクセスを与えてはいけません。起動時と毎分の検査は一般的な接続先を対象とし、無条件の隔離を保証しません。未識別名の abstract socket、別パスのエンジンソケット、特権ブローカー、その他の権限昇格経路、ホスト root の侵害、gateway 内ですでに動いている悪意あるコードは対象外です。検査の間に新しいアクセス経路が生じる可能性もあります。実行ファイルの所有者検査はサービス uid による改変を防ぐもので、悪意ある root 管理者が渡すファイルの署名検証ではありません。

この鍵は汎用サンドボックスではありません。鍵を保持する間は gateway 内の ONNX 読み込み禁止も適用され、NER マスキング、ローカル embedding、Kokoro は使えません。必要に応じて正規表現のマスキングを使用してください。データとルートキーの両方を含むバックアップは認証情報を復号できます。ダッシュボードの保護キーだけを替えても旧データの復元は防げません。漏えいが疑われる場合、ホスト管理者はサービスを停止し、安全にルートキーを交換し、保護の再設定とアカウントの再接続を行ってください。

ネイティブ systemd のインストール、sudo の各バージョン、ソケット検査は対象ディストリビューションでの実測が必要です。単体テストや macOS のコンパイル成功を Linux 実機の検証と同一視しないでください。

## エンジン制御ソケットの公開後に復旧する

同じホストの別のプログラム（開発者が起動したコンテナなど）が、root 所有のエンジン制御インターフェースを公開することがあります。`duduclaw` が Docker グループに属していなくても起こり得ます。警告と認証情報の保護状態には、パスと条件（書き込み可能、接続成功、予期しない errno）、または abstract socket 名と peer uid を表示します。直近の失効理由、詳細、UTC 時刻を `<home>/state/root-key-status.json` に保存します。`<home>` は通常 `/var/lib/duduclaw/.duduclaw` です。鍵そのものは記録しません。表示されたインターフェースを公開したプログラムを特定して停止してください。開発用コンテナは別のホストで動かすことを推奨します。古い containerd shim が検出された場合は containerd を更新してください。

自動復旧は既定で有効ですが、root 所有の再読み込み成功マーカーが unit と完全に同じ内容を持つことに加え、起動上限と終了コード 75 の再起動設定を含め、インストール済みの root 所有 unit が現在のテンプレートと完全に一致することを先に確認します。欠落、読み取り失敗、不一致の場合は自動再起動を無効にし、管理者による `service upgrade --protected-linux` が必要と表示します。アップグレードは内容が異なる unit とランチャーを原子的に置換し、`daemon-reload` を実行します。インストールとアップグレードでは unit 変更前にマーカーを無効にし、`daemon-reload` 成功後、root が `systemctl show` で drop-in を含む実効の起動間隔、起動回数上限、強制再起動の終了コードを検証した場合だけ原子的に書き込みます。検証に失敗するとマーカーは作成されず、自動復旧は無効のままです。既存のインストールもこの手順が必要です。自動復旧の対象は `engine_socket_reachable` だけです。失効後も同じ検査を 60 秒ごとに続け、3 回連続で成功すると通常の終了処理を 1 回要求します。途中で失敗すると回数をリセットします。gateway は終了処理の後に終了コード 75 で停止し、systemd が root ランチャーを再起動します。ランチャーと新しい gateway はすべての鍵受け取り検査を再実行し、成功した場合だけ鍵を受け取ります。拒否されたプロセス内で鍵を復元せず、鍵を保持しているプロセスでは復旧再起動を行いません。実行ファイルの所有者や権限、補助グループ、cgroup／unit、user namespace、コンテナ環境、鍵パスへのアクセスの問題は管理者による修復が必要で、自動復旧の対象外です。

時間あたりの上限は root 管理の systemd unit の `StartLimitIntervalSec=3600` と `StartLimitBurst=4` が強制します。systemd の計数区間で起動は合計 4 回までで、復旧、通常の障害、手動起動で枠を共有します。最初の起動も 1 回に数えるため、最初の区間では追加 3 回まで、次の区間では最大 4 回です。通常の障害用の別枠は設けません。`start-limit-hit` に達すると自動再起動は止まり、待つだけでは次の試行は予約されません。原因を修復した後、ホスト管理者が制限の許す時点でサービスを起動するか、明示的に失敗状態をリセットする必要があります。[systemd の起動制限](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html#StartLimitIntervalSec=interval)も参照してください。`<home>/state/root-key-recovery.json` の直近 1 時間の予算、ロック、診断履歴は補助情報です。同じ uid の社員は変更や書き込み妨害ができますが、systemd の計数はリセットできません。状態ファイルは固定したディレクトリ記述子から開き、シンボリックリンクを追わず、所有者、モード、単一ハードリンクを検査します。診断は既知の理由と復旧状態、有効な RFC3339 時刻、エスケープ後最大 512 bytes の詳細のみを受け付け、CLI、ログ、RPC では端末制御文字や不可視の書式文字を可視のエスケープ表記にします。状態ファイルを読み書きできない場合は自動復旧を止め、個人接続の停止を維持します。報告されたエラーからストレージとアクセス権を確認してください。再起動中は他の gateway 接続も短時間中断します。手動で再起動する従来の動作を使う場合は `config.toml` に次を設定します。

```toml
[credential_protection]
auto_recover = false
```

新しくインストールした unit には `Restart=on-failure` に加えて `RestartForceExitStatus=75`、そして `[Unit]` に上記 2 項目の起動制限が含まれます。既存の保護更新コマンドは実行ファイルだけを置き換え、ランチャーや unit は書き換えません。変更していない旧 unit も `Restart=on-failure` で終了コード 75 を再起動できますが、実行ファイルの更新だけでは新しい時間制限は適用されず、unit の更新が必要です。ホスト管理者は systemd の計数をリセットできますが、home の JSON ではできません。新しいリリースを検証してインストールした後、ホスト管理者は `sudo /opt/duduclaw/bin/duduclaw service install --protected-linux` で組み込みのランチャーと unit を更新できます。データとルートキーを維持し、systemd を再読み込みし、稼働中の gateway を再起動します。ローカルの unit 上書き設定も確認してください。
