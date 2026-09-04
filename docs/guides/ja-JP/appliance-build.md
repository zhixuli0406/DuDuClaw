# DuDuClaw OS imageのビルド

> **移動しました（2026-09）。** OS imageはこのリポジトリからはビルドされ
> なくなりました。このページがかつて説明していたYocto layer、リリース
> パイプライン、そして凍結済みのDebian/mkosiアプライアンスrecipeは、すべ
> て独立した[DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS)リポ
> ジトリに移っています。このページはプラットフォーム文書からの安定した
> 入口として残り、行き先を案内します。

DuDuClaw OSは、小型のx86-64 PCをヘッドレスなDuDuClawアプライアンスに変える
起動用imageです（完成品がユーザー側からどう見えるかは
[アプライアンス機能の概要](../../features/ja-JP/50-duduclaw-os-appliance.md)
を参照してください）。いま読んでいるこのプラットフォームリポジトリには、
OSがトリミング済みスナップショットとしてvendorするRust workspaceが入って
います。OS自体は独自のリポジトリ、独自のリリースライン、独自の
changelogを持っています。

---

## 1. imageを入手する

方法は2通りあります。

- **署名済みリリースをダウンロードする。**
  [DuDuClaw-OS Releases](https://github.com/zhixuli0406/DuDuClaw-OS/releases)
  では、マシンごとにディスク全体のimage
  （`duduclaw-os-<machine>-v<ver>.wic.zst`）とliveインストーラーISO
  （`duduclaw-os-installer-<machine>-v<ver>.iso`）が公開されており、それ
  ぞれに`.sha256`とminisignの`.minisig`が付いています。書き込む前に必ず
  検証してください。公開鍵と具体的なコマンドは、そのリポジトリのREADME
  （「快速開始」／"Quick start"）と`SECURITY.md`にあります。
- **ソースからビルドする。** DuDuClaw-OSをこのリポジトリの隣にcloneし、
  そのREADME（「從原始碼建置」／"Build from source"）と
  `meta-duduclaw/README.md`（"Usage"）に従います。Dockerビルダーコンテナ、
  `kas build`、そして`scripts/release-os.sh build → smoke → package →
  publish`というパイプラインです。このプラットフォームリポジトリの
  sibling checkoutが必要になるのは、vendorされているRustスナップショット
  を更新したいときだけです。

## 2. ビルドしているものは何か

出荷されるimageは、`meta-duduclaw/`のYocto layer（Yocto 6.0
"wrynose"、kernel 6.18）から作られる`duduclaw-image-appliance`です。A/B
デュアルスロット構成でアトミックな更新とロールバックに対応し、読み取り
専用ルートはdm-verityで検証され、自己署名Secure Bootはスロットごとに
二重署名されたUKIを使い、DuDuClaw gateway＋dashboardのpayloadを含みます。
定義されているマシンは2つです。`duduclaw-qemux86-64`（QEMUの
bring-upターゲットで、起動を検証済み）と`duduclaw-genericx86-64`（実機
x86-64ハードウェアで、設定は監査済み。実機での起動検証は今も未解決の
項目です）。image ごとの役割分担とパーティション／起動チェーンの詳細は
OSリポジトリ側に記載されており、ここでは重複させません。

## 3. Debian/mkosiのライン

このページが元々説明していた`appliance/`のrecipe（Debian 13＋mkosi、
自己インストール型USB image）は**凍結**されています。DuDuClaw-OSリポジ
トリの`appliance/`配下に、参照用および移行期の成果物として保管されて
おり、出荷対象ではなく、修正も受けません。そこにある独自のREADMEには、
経緯を確認したい人のために完全な起動シーケンスと未解決の論点がそのまま
残されています。

## 関連ページ

- [DuDuClaw OSアプライアンス](../../features/ja-JP/50-duduclaw-os-appliance.md)
  ―― 完成した機器がユーザー側からどう見え、何をするか。
- [ハードウェア要件と互換性](hardware-requirements.md) ―― どんな
  ハードウェアで動くか、起動メディアの書き込み方。
- [DuDuClaw-OSリポジトリ](https://github.com/zhixuli0406/DuDuClaw-OS)
  ―― layer、パイプライン、changelog、そしてドキュメント索引
  （`docs/README.md`）。
