# ワンショットPTY呼び出し（と、削除されたプール）

> パイプ相手には喋らないCLIがある。DuDuClawは本物の端末を用意してやる。

---

## 今あるもの

エージェントの返信は毎回AI CLIを新しくspawnします。`claude -p "<prompt>"`を実行し、答えを読み、終了する——これが`FreshSpawn`パスであり、唯一のパスです。

多くの場合CLIはただのサブプロセスですが、出力が本物の端末に向いているかを確認し、そうでなければ対話実行を拒むCLIもあります。その場合DuDuClawは**疑似端末**を確保し（`portable-pty`: Windows 10 1809+ではConPTY、macOSとLinuxではopenpty）、その中でCLIをspawnし、プロセス終了までstdoutを読み切ります。1回の呼び出し、1つのプロセス、状態は残りません。

```
gateway
   │
   ▼
invoke_oneshot(program, args, env, cwd, deadline, clear_env)
   │
   ├─ PTYを確保（ConPTY / openpty）
   ├─ その中でCLIをspawn —— CLIにはTTYが見える
   ├─ EOFまでstdoutを読み切る（またはdeadlineが子プロセスを殺す）
   ▼
取得したstdout
```

機能はこれで全部です。現在の利用者: Grokランタイム（そのCLIはTTYを要求します）と、`duduclaw`自身のCLIログイン補助フロー。

知っておくべき挙動が2つあります:

- **`clear_env`** —— 有効にすると子プロセスは空の環境から始まり、呼び出し側が明示した許可リスト（＋`NO_COLOR` / `TERM`）しか見えません。gateway自身のベンダーAPIキーをspawnされたエージェントCLIから遮断しているのがこれです（credentials doctrine P3）。
- **`deadline`** —— 絶対的なwall-clock上限。期限までに終了しない子プロセスは殺され、呼び出しはread-timeoutエラーを返します。

---

## 2026-09に削除: PTYセッションプール

v1.65まで、このページははるかに大きなものを説明していました。**長寿命の対話型`claude` REPLセッション**のプール、応答の開始と終了をランタイムが判別するためのin-bandセンチネルフレーミング、エージェント単位の`[runtime] pty_pool_enabled`オプトイン、supervisorとSIGTERM→SIGKILLのシャットダウン連鎖を伴うプロセス外`duduclaw-cli-worker`、降格ブレーカー、`GET /api/runtime/status`エンドポイント、そして`pty_pool_*` Prometheusカウンター一式。およそ8,000行です。

すべて削除しました。理由は2つ:

**1. 備えていた事態が来なかった。** このプールは、AnthropicがOAuthサブスクリプションアカウントの`claude -p`を封鎖した場合に、フラグ1つでチャンネル返信を維持するために存在していました。Anthropicはまさにその変更を2026-06-15に予定し——当日に停止しました。15か月経った今もOAuthサブスクリプションで`claude -p`は動き、この保険は返信パスを横切るリファクタのたびに保守コストを請求していました。

**2. そもそも安全に有効化できなかった。** プールのセッションキーは`(agent, cli_kind, bare_mode, account, model)`で、**会話の次元がありません**。1つのエージェントが2つのWebChat会話を担当すると単一のライブREPLを共有し、そのREPLは自分の過去のターンを覚えているため、会話Bが会話Aの作業状態を見てしまいます。このページ自身「有効にする前に読むこと」という見出しでそう書いていました。誰も責任を持って有効化できない機能は備えではなく、フラグの付いた未完成品です。

現場の事故もありました。ダッシュボードのバグが同意なく`agent.toml`に`pty_pool_enabled = true`を書き込み、本番インストールを対話パスに乗せてしまったのです——単一OAuthアカウントが取り合いになるとそこで停滞します。これを元に戻すために一度きりの起動時マイグレーション（`wp10-pty-default-reset`）が必要でした。そのマイグレーションも削除済みです。該当したインストールはとうに実行済みで、それが直した設定キー自体がもう存在しません。

### 影響

明示的にオプトインしていたのでなければ、影響はありません。`agent.toml`に`[runtime] pty_pool_enabled` / `worker_managed` / `pty_idle_timeout_secs` / `pty_interactive_timeout_secs`が残っていても、これらは無視されます——未知のキーは許容されるので壊れません。都合の良いときに削除してください。`DUDUCLAW_DISABLE_PTY_POOL`のkill switch、`/api/runtime/status`エンドポイント、`pty_pool_*` / `worker_*`メトリクスは消えました。`DUDUCLAW_PTY_DISABLE_RETRY`はワンショットパスで引き続き有効です。

### Anthropicが分割を再開したら

そのときは作り直します——意図的に、最初のコミットからセッション同一性に会話の次元を入れて。設計ノートは`commercial/docs/runtime-pty-pool-design.md`にあり、削除した実装はv1.65タグのgit履歴に残っています。

---

## まとめ

本物の端末を要求するCLIを、DuDuClawは疑似端末経由で、1回の呼び出しにつき1回spawnして駆動します。その上に載っていた常駐REPLプールは、停止されたまま再開されなかった方針変更への保険であり、かつ会話間コンテキスト漏れという欠陥を抱えて有効化できないものでした。8,000行の備えを誠実に保つコストは、その日が来てから作り直すコストを上回ります。
