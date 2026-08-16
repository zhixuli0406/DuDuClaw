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

- **ライセンス**：`license.json` はマシンフィンガープリント（hostname + MAC）に紐づいている。
  マシンを変更してもPersonal Editionのコア機能はそのまま動作する（Apache 2.0）。Pro付加モジュール
  がある場合は、[spec-license-module.md](../../../commercial/docs/spec-license-module.md) §7.3の
  セルフサービス再紐付けフローに従うこと。
- **チャンネルトークン**：channel bot tokenは暗号化された設定内にあり、一緒に移行される。IPや
  ドメインを変更した際は、webhook URLの更新を忘れないこと。
- **EditionProfile**：セルフホストのデフォルトは `personal`。`DUDUCLAW_EDITION` 環境変数または
  `agent.toml [edition] profile` で上書きできる（優先順位は
  [personal-edition-plan.md](../../../commercial/docs/personal-edition-plan.md) §4を参照）。

## ロードマップ（計画中）

- ダッシュボードに「ワンクリックでデータをエクスポート」（一鍵匯出我的資料）ボタンを追加し、
  tarballを生成する。
- 起動時にマネージドからエクスポートされたtarballをワンクリックでインポートできるようにする。
- マネージド ↔ セルフホストのラウンドトリップ整合性を自動検証する。

追跡先：`commercial/docs/TODO-personal-edition.md` のP4項目。
