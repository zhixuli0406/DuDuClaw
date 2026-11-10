# アップデート後の macOS フォルダアクセス

言語：[English](../macos-folder-access.md) · [繁體中文](../zh-TW/macos-folder-access.md) · 日本語

macOS はプライバシーの「ファイルとフォルダ」で一部のフォルダを保護しています：
`~/Desktop`（デスクトップ）、`~/Documents`（書類）、`~/Downloads`（ダウンロード）、
iCloud Drive（`~/Library/Mobile Documents`）、クラウドストレージのフォルダ
（`~/Library/CloudStorage`）、外部ボリュームとネットワークボリューム（`/Volumes/…`）。
アプリはこれらを読み取る前に、一度許可される必要があります。

## 起きること

DuDuClaw のアップデート後、macOS が改めて確認することがあります。Mac の前で誰も「許可」をクリックしないと：

- AI スタッフはこれらのフォルダ内のファイルを読み取れません。読み取りはすぐに失敗するか、macOS の確認ダイアログが表示されている間は時間切れまで待ち続けます。
- ダッシュボードには、影響を受けるフォルダを示すバナーが管理者に表示されます。
- チャットチャネルを連携している管理者には、状態が変わったとき（待機中 → 拒否 → 復旧）に**一度だけ**メッセージが届きます。同じ状態が繰り返し通知されることはなく、おやすみ時間中のメッセージは時間帯の終了後に送られます。
- ダッシュボードのヘルスチェックに「macOS フォルダアクセス」の行が表示されます。
- ファイルツールは一般的な失敗ではなく具体的な説明を返し、macOS が許可を待っている間に時間切れになったチャットの返信でも理由が示されます。

DuDuClaw が確認するのは実際に使うフォルダだけです：`DUDUCLAW_HOME`、`[files] allowed_roots`
の各項目、委任タスクの作業フォルダ。確認のタイミングは、起動時（アップデート後の再起動）、
ダッシュボードで `[files] allowed_roots` を保存した直後、そして許可されていない間は 10 分ごとです。
確認ダイアログを出さずに現在の状態を問い合わせる方法は macOS にないため、各フォルダを実際に読み取って確認します。

## アップデート後に確認される理由

LaunchAgent はゲートウェイのプログラムを直接実行するため、macOS はゲートウェイを
アクセスを求めるプログラムとして扱います。ゲートウェイ自身と、それが起動するすべての AI ツールの分です。

v1.81.0 から、macOS のゲートウェイは署名・公証済みのアプリ `DuDuClaw Gateway.app`
（識別子 `ai.duduclaw.gateway`、CE と Pro で共通）として提供されます。アップデート前後で同一性が保たれるため、
macOS はフォルダ、オートメーション、フルディスクアクセスの許可を保持します。一度許可すれば、以降のアップデートで再び確認されることはありません。

単体のコマンドラインバイナリのまま動いているゲートウェイ（v1.81.0 より前にインストールし、まだ移行していないもの）は
固定のアプリ署名を持たないため、アップデートのたびに macOS からは新しいプログラムに見え、アップデートごとに一度確認されます。
次の 3 つの方法でアプリに移行できます。

- 自動更新が有効で、ログインサービス（launchd）から起動している場合、v1.81 の初回起動時に一度だけ自動で移行します。
- ダッシュボードの「システム → アップデート」ページで「DuDuClaw Gateway.app に移行」を押します（管理者）。
- ターミナルで `duduclaw service migrate-bundle` を実行し、続けて `duduclaw service restart` を実行します。

移行すると元のバイナリはアプリへのリンクに置き換わり、既存のコマンドはそのまま使えます。移行直後に、macOS は新しいアプリについて
フォルダアクセスを**一度だけ**確認します。アプリ更新時の確認内容は[ダッシュボード設定](dashboard-settings.md)を参照してください。

## 再び許可する方法

1. DuDuClaw を実行している Mac で、ダイアログの「許可」をクリックします。
2. ダイアログを見逃した、または「許可しない」を選んだ場合：
   「システム設定 › プライバシーとセキュリティ › ファイルとフォルダ」を開き、**DuDuClaw Gateway**
   （単体バイナリのまま動いているゲートウェイの場合は **duduclaw**）の
   必要な項目（書類、デスクトップ、ダウンロード、ネットワークボリューム、リムーバブルボリューム）をオンにします。
3. 再起動は不要です。DuDuClaw が待っていたダイアログに回答すると数秒以内に検出され、
   システム設定での変更は次の 10 分ごとの確認で検出されます。その後バナーは消え、管理者には「復旧」のメッセージが届きます。

## そもそも確認を避ける：ファイルの置き場所

いちばん簡単なのは、DuDuClaw が使うファイルを保護されたフォルダに置かないことです。

- **AI スタッフの作業ファイル：** `~/DuDuClaw-files` のような通常のフォルダを作ってファイルを移し、
  ダッシュボードの設定で `[files] allowed_roots` に追加します。ホームフォルダ直下のフォルダは保護されていません。
- **`DUDUCLAW_HOME`：** 既定の `~/.duduclaw` のままにしてください。書類、デスクトップ、iCloud Drive には置かないでください——
  iCloud はファイルをクラウドに退避させることがあり、データベースが壊れる原因になります。
- **共有ドライブ：** ネットワークボリュームや USB ボリュームは常に保護対象です。どうしても使う場合は、
  DuDuClaw Gateway の「ネットワークボリューム」／「リムーバブルボリューム」を一度許可してください。単体バイナリのまま動いているゲートウェイは、アプリに移行するまでアップデートのたびに許可が必要です。

## フルディスクアクセス（任意）

「システム設定 › プライバシーとセキュリティ › フルディスクアクセス」でも動作しますが、範囲がはるかに広くなります：
DuDuClaw——およびそれが起動するすべての AI ツール——が、メール、メッセージ、他のアプリのデータを含む
Mac 上のすべてを読み取れるようになります。上記のフォルダごとの設定をおすすめします。

既知の問題：macOS 26.1 と 26.2 では、コマンドラインのバイナリ（`DuDuClaw Gateway.app` にまだ移行していないゲートウェイ）に
与えたフルディスクアクセスが確実には反映されません。許可しても読み取れない場合は、ゲートウェイをアプリに移行するか、フォルダごとの設定を使ってください。

## 企業向け：PPPC プロファイルで事前に許可する

管理対象の Mac では、MDM からプライバシー設定ポリシーコントロール（PPPC）プロファイルを配布して、
フォルダアクセスを事前に許可できます。**これは `DuDuClaw Gateway.app` にのみ適用されます**
（Team ID `7469HYQ6HH`、識別子 `ai.duduclaw.gateway`）。単体のコマンドラインバイナリのまま動いているゲートウェイには
プロファイルが照合できる固定の署名がないため、先にアプリへ移行してください。

プロファイルの全文例は[英語版](../macos-folder-access.md#enterprise-pre-approve-with-a-pppc-profile)を参照してください
（XML は言語に依存しません）。要点は、`com.apple.TCC.configuration-profile-policy` の `Services` に、
必要な項目（`SystemPolicyDocumentsFolder`、`SystemPolicyDesktopFolder`、`SystemPolicyDownloadsFolder`、
`SystemPolicyNetworkVolumes`、`SystemPolicyRemovableVolumes`）ごとに次のエントリを追加することです：

```xml
<dict>
  <key>Identifier</key><string>ai.duduclaw.gateway</string>
  <key>IdentifierType</key><string>bundleID</string>
  <key>CodeRequirement</key>
  <string>identifier "ai.duduclaw.gateway" and anchor apple generic and certificate leaf[subject.OU] = "7469HYQ6HH"</string>
  <key>Authorization</key><string>Allow</string>
</dict>
```

チームが実際に使うフォルダだけを許可してください。

## macOS では npm ではなく install.sh でインストールする

`install.sh` は `DuDuClaw Gateway.app` を `~/Applications/` にインストールし、CLI はアプリへのリンクから起動します。
旧バージョンのアップデートに必要な単体のバイナリも残します。インストール前に、minisign でリリースの署名を確認し、
アプリの開発者署名、識別子、バージョン、エディションも確認します：

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

`install.sh` には [minisign](https://jedisct1.github.io/minisign/)（`brew install minisign`）が必要です。
見つからない場合は確認を省略せず、インストール方法を示して停止します。

npm パッケージはプラットフォームパッケージ内の、Node.js のバージョンを含むフォルダ
（例：`…/node/v22.x/lib/node_modules/…`）にアプリをインストールするため、Node のアップグレードや
Node のバージョン管理ツールの利用でアプリの場所が変わります。`install.sh` は常に同じ場所を使います。
