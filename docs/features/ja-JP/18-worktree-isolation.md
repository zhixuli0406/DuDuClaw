# Git Worktree L0 分離

> 軽量サンドボックス——タスクごとに1つのファイルシステム、アトミックなマージ、コンテナ不要。

---

## たとえ話：レストランのミザンプラス（仕込み台）

営業前、シェフは各ステーションをセットアップします——それぞれ専用のまな板、包丁、仕込み済みの食材を持ちます。別々の料理を担当する2人の料理人が、同じエシャロットに手を伸ばしたり、玉ねぎを互いのまな板に削り落としたりすることはありません。

料理が完成すると、皿はパスに送られ、ステーションは次の注文のために片付けられます。料理人が調理中に料理を台無しにしても、そのステーションは片付けられますが——厨房の残りの部分は稼働し続けます。

コンテナサンドボックスは、料理人ごとに*別個の厨房*をまるごと建てるようなものです——高価で、単純な仕込みにはやり過ぎです。Git worktree 分離は、同じ厨房の中で各料理人に専用のステーションを与えるだけです。安く、速く、それでいて肝心なところは分離されています。

---

## なぜブランチだけでなく Worktree なのか

複数の並行エージェントが*同じ* checkout を編集すると、互いのファイルを上書きしてしまいます。明白な解決策は、各エージェントに専用のブランチを与えることです。しかし単一ブランチは依然として1つの作業ディレクトリにすぎません——ブランチを切り替えると未コミットの作業が踏みつぶされます。

`git worktree` は、各ブランチに**専用のファイルシステムディレクトリ**を与えることでこれを解決します：

```
Main repo:        /Users/you/project/
                  ↓
Branch main:      /Users/you/project/                  ← always here
Worktree 1:       /Users/you/project-wt/agent-a-swift-fox/
Worktree 2:       /Users/you/project-wt/agent-b-calm-pine/
Worktree 3:       /Users/you/project-wt/agent-c-eager-leaf/
```

3つのディレクトリは同じ `.git/` オブジェクトストアを共有する（そのためストレージ効率が良い）一方で、それぞれが独自の作業ファイルと HEAD を持ちます。3つのエージェントが衝突なく同時に編集できます。

DuDuClaw はこれを **L0 分離**と呼びます——L1 コンテナサンドボックスより安く、単なる「エージェントが衝突しないことを祈る」より強固です。

---

## WorktreeManager

`WorktreeManager` はフルライフサイクルを提供します：

```
Agent task starts
     |
     v
create(agent_id, task_id)
     |
     v
├─ Generate branch name: wt/{agent_id}/{adjective}-{noun}
├─ Check resource limits (max 5/agent, 20 total)
├─ git worktree add <path> <branch>
├─ copy_env_files(.env, config.local, ...)
└─ Return WorktreeHandle
     |
     v
[Agent executes — reads/writes ONLY in its worktree]
     |
     v
inspect() — did the execution succeed?
     |
     v
merge() or cleanup() based on AgentExitCode
```

### 親しみやすいブランチ名

ブランチ名は 50×50 のワードペアジェネレーターから生成されます——形容詞 + 名詞：

```
wt/duduclaw-pm/swift-fox
wt/xianwen-coder/calm-pine
wt/agnes/bright-hawk
wt/sam/crisp-river
```

単語は短く、覚えやすく、不快でないように厳選されています。2,500通りの組み合わせ × エージェントスコープでは、衝突は稀でクリーンアップも容易です。

### リソース上限

```
MAX_WORKTREES_PER_AGENT: 5
MAX_TOTAL_WORKTREES:     20
```

エージェントが上限に達した場合、worktree が静かに蓄積してディスクを使い果たすのではなく、新しい worktree の作成は即座に失敗します。

---

## Snap ワークフロー

Worktree は空間を*作る*ためだけのものではありません——**制御されたマージプロトコル**も定義します。agent-worktree プロジェクトに着想を得て、DuDuClaw は4段階のワークフローを使用します：

```
┌─────────────────────────────────────────────────────┐
│  1. CREATE                                          │
│     └─ worktree + branch + env file copy            │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  2. EXECUTE                                         │
│     └─ Agent does work inside the worktree          │
│        (container sandbox nested here if needed)    │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  3. INSPECT                                         │
│     └─ Pure decision function (testable)            │
│        AgentExitCode + diff + test results          │
│        → Decide: merge / cleanup / keep-alive       │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  4. MERGE or CLEANUP                                │
│     └─ Atomic merge with dry-run pre-check          │
│        or remove worktree + branch                  │
└─────────────────────────────────────────────────────┘
```

純関数の判断ロジック（ステージ3）は I/O から分離されているため、フィクスチャを使って単体テストできます——テストに git 操作は不要です。

### 構造化された終了コード

`AgentExitCode` は型付き列挙型なので、呼び出し側が数値コードを検査する必要はありません：

```
AgentExitCode {
  Success,       → auto-merge (if configured)
  Error,         → cleanup, don't merge
  Retry,         → keep worktree, re-run
  KeepAlive,     → leave for manual inspection
}
```

---

## 事前チェック付きのアトミックなマージ

マージステップは最も危険です——2つの並行エージェントが同時に `main` へマージしようとすると git の状態が破損します。DuDuClaw は**グローバルなマージミューテックス**と **dry-run 事前チェック**でこれを解決します：

```
merge(worktree_handle):
     |
     v
acquire global_merge_lock()        ← serializes all merges
     |
     v
git merge --no-commit --no-ff <branch>   ← dry-run
     |
     v
if conflicts or errors:
    git merge --abort
    return MergeResult::Conflicts(info)
     |
     v
git merge --abort                  ← still dry-run, rollback
git merge <branch>                 ← now commit for real
     |
     v
release global_merge_lock()
```

`Mutex<()>` は `OnceLock<Mutex<()>>` の中に存在するため、全スレッドにわたる**すべての** `WorktreeManager` インスタンスが同じロックを共有します。これがないと、異なる async タスクからの `dispatch_in_worktree` 呼び出しがそれぞれ独自の manager を作成して競合します。

---

## copy_env_files：機密情報の安全なコピー

worktree が作成されるとき、メイン repo の特定のファイルを一緒に持っていく必要があります——通常は `.env`、`.env.local`、または設定ファイルです。しかし素朴なコピーは3つの攻撃面を開きます：

1. **パストラバーサル**——`.env/../../etc/passwd`
2. **シンボリックリンク**——`.env → /etc/shadow`
3. **巨大なファイル**——500MB もある `.env`

`copy_env_files` はこの3つすべてを封じ込めます：

```
for each file in allowlist:
     |
     v
canonical_path = fs::canonicalize(main_repo_path + file)
     |
     v
if !canonical_path.starts_with(main_repo_path):
    reject (path traversal)
     |
     v
if fs::symlink_metadata(canonical_path).is_symlink():
    reject (symlink attack)
     |
     v
if fs::metadata(canonical_path).len() > 1 MB:
    reject (oversize)
     |
     v
copy to worktree_path/file
```

これを一度でも誤れば、ホストの認証情報をエージェントのサンドボックスに晒す可能性があります——だからこそ封じ込めは厳格です。

---

## Agent ID のサニタイズ

Agent ID はユーザー入力から来ます（`agent create my_agent!!!`）。ブランチ名には厳格な文字セットのルールがあります。ブランチ名を形成する前に：

```
sanitize("My_Agent!!!")
     |
     v
lowercase     → "my_agent!!!"
     |
     v
replace [^a-z0-9-] with '-'
              → "my-agent---"
     |
     v
collapse multiple -
              → "my-agent-"
     |
     v
strip leading/trailing -
              → "my-agent"
```

これにより、不正な形式のブランチ名が git を壊すのを防ぎます。

---

## 分離階層における Worktree の位置づけ

DuDuClaw には複数の分離レイヤーがあります——十分なもののうち最も安いものを使いましょう：

| レイヤー | 仕組み | コスト | 分離対象 |
|-------|-----------|------|----------|
| **L0 Worktree** | `git worktree` | 非常に低い | 並行エージェント間の作業ファイル |
| **L1 Container** | Docker / Apple Container / WSL2 | 中 | ファイルシステム + ネットワーク + プロセスツリー |
| **L2 Capability Deny** | `agent.toml [capabilities]` | なし（ポリシー） | ツールアクセス（bash / browser / computer use） |
| **L3 Hooks** | Claude Code PreToolUse hooks | 低 | 実行時のコマンドレベルのブロック |

純粋な「この関数をリファクタする」タスクには L0 だけで十分です——エージェントはファイルを編集し、ネットワーク呼び出しはありません。「依存関係をインストールしてテストを実行する」タスクには L0 + L1 が必要です——ファイルシステム分離*に加えて*コンテナのネットワーク境界です。

---

## 設定

`agent.toml` でエージェントごとに：

```toml
[container]
worktree_enabled       = true
worktree_auto_merge    = true   # auto-merge on Success exit code
worktree_cleanup_on_exit = true # remove on Error/Success
worktree_copy_files    = [".env", ".env.local", "config.local.json"]
```

`worktree_enabled = false` の場合、タスクはメインの checkout ディレクトリで実行されます（過去の挙動で、単一エージェントのデプロイには許容範囲）。

---

## 可観測性

`WorktreeManager` はすべてのライフサイクルステージで tracing スパンを発行します：

```
INFO  worktree::create{agent_id=dudu branch=wt/dudu/swift-fox}
DEBUG worktree::copy_env_files{files=[.env, .env.local]}
INFO  worktree::execute_start{task_id=t_abc123}
INFO  worktree::execute_end{exit_code=Success duration=12.3s}
INFO  worktree::merge{dry_run=true conflicts=0}
INFO  worktree::merge{committed=true hash=abc1234}
INFO  worktree::cleanup{removed_branch=true removed_dir=true}
```

これらは BroadcastLayer → WebSocket → Dashboard Logs ページを通じて流れるため、worktree の起動とマージをリアルタイムで観察できます。

---

## なぜ重要か

### コンテナなしの並行性

かつて5つのエージェントを同時に実行するには、次のどちらかを意味していました：
- 1つのメイン checkout で、エージェントがファイル上で衝突する（速いが壊れる）、または
- 5つのコンテナサンドボックス（分離されているが起動コストが高い）。

Worktree は、ほぼ前者のコストで後者の分離を提供します。

### アトミックで監査可能なマージ

dry-run 事前チェックにより、壊れたマージが中途半端に適用されることは決してありません。マージはきれいに成功するか、完全に中止されるかのどちらかです。混在状態はありません。「一部のファイルはマージされ一部はされなかった」もありません。

### 他のレイヤーと組み合わせ可能

エージェントはコンテナサンドボックスの*内部*にある worktree で実行できます——worktree はファイルシステム分離を、コンテナはネットワーク／プロセス分離を提供します。これらは積み重ねられます。

### 人間に優しいブランチ名

`wt/dudu-pm/swift-fox` は覚えやすく、ログで grep もしやすいです。`wt-3d8b2a...` のような不透明なハッシュより優れています。

---

## まとめ

5つのエージェントが同じコードベース上で同時に作業するには、*何らかの*分離が必要です。コンテナはほとんどのタスクにはやり過ぎです。生のブランチでは不十分です。Git worktree はスイートスポットを突きます——安く、並行的で、メイン repo を破損させないアトミックなマージプロトコルを備えています。これはすべての DuDuClaw エージェントタスクのデフォルト分離レイヤーです。
