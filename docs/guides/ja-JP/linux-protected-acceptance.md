# Linux 保護モード：受け入れ手順書（v1.82.0 で提供予定）

[English](../linux-protected-acceptance.md) · [繁體中文](../zh-TW/linux-protected-acceptance.md)

> **状態：v1.82.0 で提供予定。v1.81.0 では利用できません。** この手順書で受け入れる社員ごとの独立した実行環境は、v1.81.0 ではお客様のホストにインストールできません。v1.81.0 の保護サービスは、v1.80 と同じ方法でインストール、更新し、社員を実行します。

リリース担当者向けの手順書です。v1.82.0 で提供予定のネイティブ Linux 隔離構成の受け入れで、準備するもの、実行するコマンド、結果の読み方を説明します。対象は個人用と共有の AI 社員、外部連携、Package H のツールです。

> **これらの受け入れツールは現在、準備段階を通過できません。** `connectors-acceptance.sh`、`package-h-acceptance.sh`、`iso_personal_chain`／`iso_package_h_chain` テストには、インストール済みで動作中の隔離コンポーネント（spawnd、egress、独立した更新サービス）が必要です。`duduclaw service install --protected-linux` はこれらをインストールせず、単一実行ファイルの保護 gateway だけをインストールします（[保護サービスガイドの「単一実行ファイルの保護サービス（現行版）」](linux-protected-service.md)を参照）。隔離構成の新規インストール手順が完成するまで（v1.82.0 予定）、毎回準備段階で停止します。この結果は AI 社員、外部連携、Package H 機能の受け入れ結果ではありません。

> **以下の手順はすべてホストを変更します。** 受け入れツールは保護モードのサービスをインストールし、システムアカウントを作り、`/var/lib/duduclaw*`・`/opt`・`/etc` に書き込み、`/etc/hosts` を変更し、使い捨ての認証局を入れ、最後にすべて削除します。使い捨ての VM だけで実行してください。`DUDUCLAW_ISO_DISPOSABLE=1` がないと実行を拒否し、ツール自身が作っていない DuDuClaw のインストールがある場合も拒否します。あなた自身の `~/.duduclaw` や Claude／agy のログインは使いません。操作用 CLI の `HOME` と `DUDUCLAW_HOME` は専用の一時ディレクトリを指します。

## 対象

| 部分 | ツール | 内容 |
|---|---|---|
| G1 | `cargo test -p duduclaw-cli`（単体テスト、Linux ビルド機、root 不要） | 本物の権限発行と本物の連携ファクトリで、263 個すべてのツールを個人用 1 名・共有 1 名について確認：経路、必要な権限、社員の種類ごとの利用可否、ファクトリの欠落なし |
| G3 | `scripts/live-test/linux-protected/connectors-acceptance.sh` | 項目 1–15：個人の紐付け、取り消し、再起動、設定が必要な状態、各連携の実地確認 |
| G2 | `iso_personal_chain` テストプログラム（root） | 個人用：申請 → 承認 → 割り当て → 所有者の連携で実行 → 停止・保管（実行中の処理も止まる）→ 復元（同じ uid）→ spawnd の状態リセット後に新しい ID で引き継ぎ（新しい uid、古いトークンは拒否）→ 消去と解放（名前を再利用でき、新しい ID と uid）。共有：停止・保管 → 復元 |
| H15 | `scripts/live-test/linux-protected/package-h-acceptance.sh` ＋ `iso_package_h_chain`（root） | 項目 16–27：ホスト情報、ブローカー経由のファイルツール、公開 Web ツールと拒否、同じ実行内の補助プログラム（305 秒以上のタイムアウト、取り消し）、テスト用スキル配布元と悪意のあるアーカイブ、D8 での委任ツール、録画の開閉、準備状態の理由 |

## 事前準備

**ホスト。** ディストリビューションごとに使い捨て VM を 1 台：Ubuntu 22.04、Ubuntu 24.04、Debian 12。root、PID 1 が systemd、cgroup v2、コンテナでないこと。パッケージ：`python3`（3.10 以上）、`util-linux`（`setpriv`）、`passwd`、`openssl`、`ca-certificates`、`coreutils`、`procps`。`/var` と `/opt` に約 2 GB。

**ビルド。** 自分で検証した候補版 `duduclaw` と、同じコミットから作った 2 つのテストプログラム：

```sh
cargo test -p duduclaw-gateway --test iso_package_h_chain --no-run
cargo test -p duduclaw-gateway --test iso_personal_chain --no-run
mkdir -p /abs/tests && cp <表示されたパス> /abs/tests/   # iso_package_h_chain / iso_personal_chain に改名
```

**ツールドライバー。** ダッシュボードの RPC は社員のツールを呼びません。実際のツール呼び出しは、あなたが用意するプログラム（`DUDUCLAW_ISO_TOOL_DRIVER`）を通します。仕様は `scripts/live-test/linux-protected/README.md` の「Tool driver contract」にあり、H15 では `seed` と `cancel` も使います。ない場合、該当する確認は `SKIP` になります。

**署名済み補助プログラム**（H21/H22）：リリースにプログラム実行・コード索引・Office の補助プログラムが含まれている必要があります。含まれていないと spawnd は `helper_calls` を出さず、確認は設計どおり拒否されます。

**テスト用サイトとスキル配布元**（H19/H20、H23/H24）：ホストに**グローバル**な IP アドレスが必要です（`--fixture-ip`）。保護モードの Web アクセスは公開アドレスにしか接続しないため、社内 LAN のアドレス（192.168.x.x など）は設計どおり拒否され、`SKIP` になります。ツールはそのアドレスで `fixtures/public_origin.py` を動かし、使い捨て認証局と `/etc/hosts` の項目（`h15.fixture-duduclaw.net`、127.0.0.1 を指す `rebind.fixture-duduclaw.net`、`clawhub.ai`）を設定します。製品の DNS と TLS が `/etc/hosts` とシステムの信頼ストアを使う場合だけ成立します。使わない場合は H19/H23 が DNS または TLS のエラーになり、本物の公開テストホスト（`DUDUCLAW_ISO_PUBLIC_ORIGIN`）が必要です。

**2 人目の管理者**（G2 の復元・消去・解放）：復元と早期消去の承認ルールには、7 日以上続けて資格のある管理者、または管理者の記録が 7 日以上あるホストが必要です。新しいホストではこれらの手順は理由付きの `SKIP` になります。確認するには長く使う使い捨てホストを用意し、2 人目の管理者のログイン状態を `DUDUCLAW_ISO_STATE_DIR/admin2.json` に置きます。

**所有者が用意する外部サービスのテストアカウント**（G3 の 1、3、7–9、14、15。G1・G2・H15 には不要）：

| サービス | 内容 | 変数 |
|---|---|---|
| Google | Workspace テストテナント：個人 OAuth の利用者 2 名、ホスト OAuth、委任またはサービスアカウント | `DUDUCLAW_ISO_GOOGLE_ALICE=1`、`DUDUCLAW_ISO_GOOGLE_HOME=1` |
| Notion | ワークスペースの認証情報 | `DUDUCLAW_ISO_NOTION=1` |
| GitHub | テスト用の組織またはリポジトリ | `DUDUCLAW_ISO_GITHUB=1` |
| LINE | Bot のチャネルトークン（Discord、Telegram も同様） | `DUDUCLAW_ISO_LINE_TOKEN`（`_DISCORD_TOKEN`、`_TELEGRAM_TOKEN`） |
| Odoo | 使い捨てインスタンスと API キー | `DUDUCLAW_ISO_ODOO_URL`（`_DB`、`_USER`、`_KEY`） |
| データベース | Postgres または MySQL の読み取り専用ユーザーと 1 つのテーブル | `DUDUCLAW_ISO_PG_DSN_RO` ＋ `_PG_TABLE`、`DUDUCLAW_ISO_MYSQL_DSN_RO` ＋ `_MYSQL_TABLE` |
| 検索 | 検索サービスのキー | `DUDUCLAW_ISO_SEARCH_API_KEY` |

OAuth は `connectors-acceptance.sh --pause-after-setup` で接続します。本物のトークンを共有のシェル履歴に残さず、`sudo env 変数=… スクリプト` で渡してください。

## コマンド

1. **G1**（Linux ビルド機、root 不要）：`cargo test -p duduclaw-cli g1_real_issuer_and_factory -- --nocapture`
2. **G3**（各ディストリビューション）：

   ```sh
   sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver \
     bash scripts/live-test/linux-protected/connectors-acceptance.sh \
     --candidate /abs/candidate-dir --out /abs/evidence/g3-ubuntu2204
   ```

3. **H15 ＋ G2**（各ディストリビューション。1 つのスクリプトで両方を実行）：

   ```sh
   sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver \
     bash scripts/live-test/linux-protected/package-h-acceptance.sh \
     --candidate /abs/candidate-dir --tests-dir /abs/tests --out /abs/evidence/h15-ubuntu2204 \
     [--fixture-ip <このホストのグローバルアドレス>] [--allow-spawnd-reset]
   ```

   `--allow-spawnd-reset` は G2 が引き継ぎを確かめるために `/var/lib/duduclaw-spawnd` を退避することを許可します。ホスト上の全社員がホスト側の割り当てを失うので、他に影響のないホストだけで使ってください。`--only h15`／`--only g2` で片方だけ実行、`--keep` で調査用にインストールを残し、`--reset` で前回の残りを削除します。

4. **spawnd と補助プログラムの root テスト**（変更なし）：`DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1` と、`crates/duduclaw-execution/tests/recording_root.rs` の録画テスト（`ISO_ROOT_CHROMIUM`）。

## 結果の読み方

- 確認ごとに 1 行：`PASS`・`FAIL`・`SKIP`、番号、説明。テストプログラムからの番号には `H15:` か `G2:` が付きます（例 `G2:G2-05a`）。最後に表と件数が出ます。
- 終了コード `1` は `FAIL` が 1 つ以上あるか、途中で中断したことを示します。`0` でも `SKIP` は残り得るので**必ず読んでください**。`SKIP` には不足しているもの（ドライバー、テスト用アドレス、2 人目の管理者、spawnd リセットの許可）が書かれ、合格には数えません。
- 「… is not implemented in this build (F6c / F6g / F6l / F6 §3.8 / F6 §3.9)」という `FAIL` は、候補版に復元・引き継ぎ・消去・解放の RPC がまだないことを示します。ホストの問題ではなく機能の不足です。
- `OUT/facts.tsv` に実行ごとのホスト情報（ディストリビューション、カーネル、systemd、Landlock ABI、LSM、spawnd の機能ビット、録画確認の有無）が残ります。3 つのディストリビューションすべてで保存してください。Ubuntu 22.04 と Debian 12 は Landlock ABI が 4 未満です（TCP ポートを制限できない既知の残存リスク）。
- `OUT/<プログラム名>.log` は全出力、`OUT/evidence/` は RPC の応答（トークンなし）・ドライバーの応答・テストサイトのアクセス記録、`journal.log` は `duduclaw*` の全ログです。
- 最後の `PASS cleanup` はインストール・サービス・アカウント・テスト用認証局・hosts の項目がすべて消えたことを示します。`FAIL cleanup` の場合は一時ディレクトリが残るので、後で `--reset` してください。

**リリース判断。** G1 が Linux ビルド機で通り、3 つのディストリビューションで G3 の † 項目と H15・G2 のすべての確認が合格、または記録され受け入れられた `SKIP` であるときだけリリースします。新しいホストで想定される `SKIP` は、G2 の復元・消去・解放（承認ルール）と、録画用補助プログラムがない場合の H26 です。録画の確認がすべてのディストリビューションで通るまで、録画は閉じたまま（`H16-recording` が PASS）でなければなりません。
